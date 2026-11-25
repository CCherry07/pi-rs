//! Idle-submission handoff to the managed whole-generation transaction.
use std::sync::Arc;

use super::AgentSession;
use crate::{SessionError, WeakPiSession};

impl AgentSession {
    pub(crate) fn bind_generation_owner(&self, owner: WeakPiSession) {
        if let Some(managed) = owner.upgrade() {
            let _ = self
                .submission_abort_epoch
                .set(managed.submission_abort_epoch());
        }
        let _ = self.generation_owner.set(owner);
    }

    pub(crate) fn generation_owner(&self) -> Option<WeakPiSession> {
        self.generation_owner.get().cloned()
    }

    pub(super) fn submission_abort_epoch(&self) -> u64 {
        self.submission_abort_epoch
            .get()
            .map_or(0, |epoch| epoch.load(std::sync::atomic::Ordering::Acquire))
    }

    pub(super) fn check_submission_epoch(&self, expected: u64) -> Result<(), SessionError> {
        if self.submission_abort_epoch() != expected {
            Err(SessionError::Cancelled("session input"))
        } else {
            Ok(())
        }
    }

    pub(super) fn replacement_generation(&self) -> Option<Arc<Self>> {
        let current = self.generation_owner.get()?.upgrade()?.current();
        (!std::ptr::eq(current.as_ref(), self)).then_some(current)
    }

    pub(crate) fn generation_is_stale(&self) -> Result<bool, SessionError> {
        self.generation_freshness
            .as_ref()
            .map_or(Ok(false), |check| check.is_stale())
    }

    pub(super) async fn refreshed_generation(&self) -> Result<Option<Arc<Self>>, SessionError> {
        if let Some(current) = self.replacement_generation() {
            return Ok(Some(current));
        }
        if !self.is_idle() || self.generation_freshness.is_none() {
            return Ok(None);
        }
        if let Some(owner) = self.generation_owner.get().and_then(WeakPiSession::upgrade) {
            owner
                .refresh_generation_if_stale(self)
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()))
        } else if self.generation_is_stale()? {
            Err(SessionError::Runtime(
                "generation refresh requires a managed session".into(),
            ))
        } else {
            Ok(None)
        }
    }
}
