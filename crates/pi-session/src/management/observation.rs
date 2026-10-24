//! One subscription follows a managed handle across generations and broadcast lag.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::{broadcast, watch};

use super::replacement::SessionSlotState;
use crate::{AgentSession, AgentSessionEvent, AgentSessionSnapshot, AgentSessionSubscription};

/// A cursor belongs to a managed handle. Revisions are comparable only within an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionVersion {
    pub epoch: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionResetReason {
    /// New, resume, fork or reload published a different runtime generation.
    Replacement,
    /// The broadcast buffer overflowed. State is restored; missed transient events are not replayed.
    Lagged { missed_events: u64 },
}

#[derive(Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "Keep product events inline to avoid allocating another box for every streamed delta"
)]
pub enum SessionUpdate {
    Event {
        version: SessionVersion,
        event: AgentSessionEvent,
    },
    Reset {
        version: SessionVersion,
        reason: SessionResetReason,
        snapshot: Box<AgentSessionSnapshot>,
    },
}

/// An initial snapshot followed by events and explicit state resets.
///
/// The observer owns generation switching, revision filtering and lag recovery.
/// It does not reduce transcript or presentation state. `recv` is cancellation-safe
/// and drains final events before returning `None` after the managed handle closes.
pub struct SessionObservation {
    replacements: watch::Receiver<Arc<AgentSession>>,
    state: Arc<SessionSlotState>,
    current: Arc<AgentSession>,
    subscription: AgentSessionSubscription,
    version: SessionVersion,
    closed: bool,
}

impl SessionObservation {
    pub(super) fn new(
        mut replacements: watch::Receiver<Arc<AgentSession>>,
        state: Arc<SessionSlotState>,
    ) -> Self {
        let (current, epoch, closed) = {
            // The epoch and close flag are published under the watch write lock.
            let current = replacements.borrow_and_update();
            (
                Arc::clone(&current),
                state.epoch.load(Ordering::Acquire),
                state.closed.load(Ordering::Acquire),
            )
        };
        let subscription = current.subscribe();
        let version = SessionVersion {
            epoch,
            revision: subscription.snapshot.revision,
        };
        Self {
            replacements,
            state,
            current,
            subscription,
            version,
            closed,
        }
    }

    /// The generation associated with the last returned update.
    pub fn current(&self) -> Arc<AgentSession> {
        Arc::clone(&self.current)
    }

    /// State captured when subscribing or at the most recent reset.
    ///
    /// Apply subsequent `Event` updates to this baseline. For a fresh read without
    /// advancing the event cursor, use `current().snapshot()`.
    pub fn snapshot(&self) -> &AgentSessionSnapshot {
        &self.subscription.snapshot
    }

    pub fn version(&self) -> SessionVersion {
        self.version
    }

    pub async fn recv(&mut self) -> Option<SessionUpdate> {
        loop {
            if let Some(reset) = self.synchronize() {
                return Some(reset);
            }
            let event = if self.closed {
                match self.subscription.events.try_recv() {
                    Ok(event) => Ok(event),
                    Err(broadcast::error::TryRecvError::Lagged(count)) => {
                        Err(broadcast::error::RecvError::Lagged(count))
                    }
                    Err(
                        broadcast::error::TryRecvError::Empty
                        | broadcast::error::TryRecvError::Closed,
                    ) => return None,
                }
            } else {
                tokio::select! {
                    biased;
                    changed = self.replacements.changed() => {
                        if changed.is_err() {
                            self.closed = true;
                        }
                        continue;
                    }
                    event = self.subscription.events.recv() => event,
                }
            };
            // A replacement may have raced with receipt of an old-generation event.
            if let Some(reset) = self.synchronize() {
                return Some(reset);
            }
            match event {
                Ok(event) if event.revision > self.version.revision => {
                    self.version.revision = event.revision;
                    return Some(SessionUpdate::Event {
                        version: self.version,
                        event: event.event,
                    });
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(missed_events)) => {
                    return Some(self.reset(SessionResetReason::Lagged { missed_events }));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    fn synchronize(&mut self) -> Option<SessionUpdate> {
        let (current, epoch, closed) = {
            let current = self.replacements.borrow_and_update();
            (
                Arc::clone(&current),
                self.state.epoch.load(Ordering::Acquire),
                self.state.closed.load(Ordering::Acquire),
            )
        };
        self.closed |= closed || self.replacements.has_changed().is_err();
        if epoch == self.version.epoch {
            return None;
        }
        self.current = current;
        self.version.epoch = epoch;
        Some(self.reset(SessionResetReason::Replacement))
    }

    fn reset(&mut self, reason: SessionResetReason) -> SessionUpdate {
        self.subscription = self.current.subscribe();
        self.version.revision = self.subscription.snapshot.revision;
        SessionUpdate::Reset {
            version: self.version,
            reason,
            snapshot: Box::new(self.subscription.snapshot.clone()),
        }
    }
}
