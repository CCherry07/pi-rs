//! Synchronous publication plus optional asynchronous acknowledgement.
//!
//! No wire format or I/O lives here. Native snapshot subscriptions remain independent.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use pi_core::{AbortHandle, AbortSignal};
use tokio::sync::Notify;

use super::events::RevisionedAgentSessionEvent;

/// A generation-local observer sees each event synchronously, outside session locks.
///
/// Reentrant publications are queued until every observer has seen the current event.
/// Concurrent publishers preserve revision order. A producer that can await calls
/// `AgentSession::flush_events` to acknowledge delivery; synchronous bursts are not
/// hard memory-bounded. Implementations own serialization, I/O and their error policy.
#[async_trait::async_trait]
pub trait SessionEventObserver: Send + Sync {
    fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String>;

    async fn flush(&self, _signal: AbortSignal) -> Result<(), String> {
        Ok(())
    }
}

/// Dropping the guard unsubscribes and releases pending acknowledgement waits.
/// Already delivered data remains owned by the observer, not this subscription.
#[must_use = "retain the guard for the lifetime of the observer"]
pub struct SessionEventObserverGuard {
    owner: Weak<EventObservers>,
    id: u64,
    release: AbortHandle,
    signal: AbortSignal,
}

impl SessionEventObserverGuard {
    /// Wait until earlier publications have reached synchronous observers, without
    /// awaiting their external I/O. Captures its checkpoint when polled.
    pub fn publication_barrier(
        &self,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send + 'static {
        let owner = self.owner.upgrade();
        let signal = self.signal.clone();
        async move {
            if let Some(owner) = owner {
                owner.pending_observers(&signal).await?;
            }
            Ok(())
        }
    }
}

impl Drop for SessionEventObserverGuard {
    fn drop(&mut self) {
        self.release.abort();
        if let Some(owner) = self.owner.upgrade() {
            owner.remove(self.id);
        }
    }
}

#[derive(Clone)]
struct RegisteredObserver {
    observer: Arc<dyn SessionEventObserver>,
    since: u64,
    release: AbortHandle,
    signal: AbortSignal,
    failure: Arc<Mutex<Option<String>>>,
}

/// Mutation owners declare this guard before taking their own locks. Publications
/// keep their original snapshot/revision order; callbacks start only after all
/// enclosing owner guards have been dropped.
pub(crate) struct ObserverDispatchGuard {
    owner: Arc<EventObservers>,
}

impl Drop for ObserverDispatchGuard {
    fn drop(&mut self) {
        self.owner.resume_dispatch();
    }
}

#[derive(Default)]
struct State {
    observers: BTreeMap<u64, RegisteredObserver>,
    next_id: u64,
    pending: VecDeque<RevisionedAgentSessionEvent>,
    dispatching: bool,
    deferrals: usize,
    published: u64,
    dispatched: u64,
    retiring: bool,
    closed: bool,
}

#[derive(Default)]
pub(super) struct EventObservers {
    state: Mutex<State>,
    progress: Notify,
}

impl EventObservers {
    pub(super) fn defer(self: &Arc<Self>) -> ObserverDispatchGuard {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deferrals += 1;
        ObserverDispatchGuard {
            owner: self.clone(),
        }
    }

    fn resume_dispatch(&self) {
        let dispatch = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.deferrals -= 1;
            if state.deferrals == 0 && !state.dispatching && !state.pending.is_empty() {
                state.dispatching = true;
                true
            } else {
                false
            }
        };
        if dispatch {
            self.dispatch();
        }
    }

    pub(super) fn register(
        self: &Arc<Self>,
        observer: Arc<dyn SessionEventObserver>,
        since: u64,
    ) -> SessionEventObserverGuard {
        let (release, signal) = AbortHandle::new();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id;
        state.next_id += 1;
        if state.closed || state.retiring {
            release.abort();
        } else {
            state.observers.insert(
                id,
                RegisteredObserver {
                    observer,
                    since,
                    release: release.clone(),
                    signal: signal.clone(),
                    failure: Arc::default(),
                },
            );
        }
        SessionEventObserverGuard {
            owner: Arc::downgrade(self),
            id,
            release,
            signal,
        }
    }

    /// Called under the event hub's publication lock, retaining its revision order.
    /// Returns whether this publisher must drain after releasing that lock.
    pub(super) fn enqueue(&self, event: &RevisionedAgentSessionEvent) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retiring || state.closed {
            return false;
        }
        state.published = event.revision;
        if state.observers.is_empty() {
            state.dispatched = event.revision;
            self.progress.notify_waiters();
            return false;
        }
        state.pending.push_back(event.clone());
        if state.dispatching || state.deferrals > 0 {
            false
        } else {
            state.dispatching = true;
            true
        }
    }

    pub(super) fn dispatch(&self) {
        loop {
            let (event, observers) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.deferrals > 0 {
                    state.dispatching = false;
                    return;
                }
                let Some(event) = state.pending.pop_front() else {
                    state.dispatching = false;
                    self.progress.notify_waiters();
                    return;
                };
                let observers: Vec<_> = state
                    .observers
                    .values()
                    .filter(|observer| event.revision > observer.since)
                    .cloned()
                    .collect();
                (event, observers)
            };
            // Neither the snapshot lock nor the observer registry lock is held.
            for observer in observers {
                if observer.signal.is_aborted()
                    || observer
                        .failure
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some()
                {
                    continue;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    observer.observer.on_event(&event)
                }))
                .unwrap_or_else(|_| Err("session event observer panicked".into()));
                if let Err(error) = result {
                    *observer
                        .failure
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                }
            }
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.dispatched = state.dispatched.max(event.revision);
            self.progress.notify_waiters();
        }
    }

    async fn pending_observers(
        &self,
        signal: &AbortSignal,
    ) -> Result<Vec<RegisteredObserver>, String> {
        let (target, observers) = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                state.published,
                state
                    .observers
                    .values()
                    .filter(|observer| observer.since < state.published)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        if observers.is_empty() {
            return Ok(Vec::new());
        }
        loop {
            let changed = self.progress.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            // The checkpoint belongs to this captured cohort. A new subscriber
            // starts after it and cannot inherit a cancelled observer's debt.
            // Check after enabling notification so a concurrent rotation cannot
            // lose the wake that releases this producer.
            if observers
                .iter()
                .all(|observer| observer.signal.is_aborted())
            {
                return Ok(Vec::new());
            }
            let ready = {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.dispatched >= target || state.closed || state.observers.is_empty()
            };
            if ready {
                break;
            }
            tokio::select! {
                () = signal.wait() => return Ok(Vec::new()),
                () = &mut changed => {}
            }
        }
        for observer in &observers {
            if observer.signal.is_aborted() {
                continue;
            }
            if let Some(error) = observer
                .failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return Err(error);
            }
        }
        Ok(observers)
    }

    pub(super) async fn flush(&self, signal: AbortSignal) -> Result<(), String> {
        for observer in self.pending_observers(&signal).await? {
            if observer.signal.is_aborted() {
                continue;
            }
            tokio::select! {
                // Abort releases the producer, not already queued frames. A live
                // observer can still receive the run's terminal cancellation events.
                () = signal.wait() => return Ok(()),
                () = observer.signal.wait() => {},
                result = observer.observer.flush(signal.clone()) => result?,
            }
        }
        Ok(())
    }

    fn remove(&self, id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = state.observers.remove(&id);
        let pending = if state.observers.is_empty() {
            std::mem::take(&mut state.pending)
        } else {
            VecDeque::new()
        };
        drop(state);
        drop(removed);
        drop(pending);
        self.progress.notify_waiters();
    }

    /// Normal retirement freezes the publication boundary, then waits only for
    /// synchronous projection of its accepted tail, not external writer I/O.
    pub(super) async fn retire(&self) -> Result<(), String> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.retiring = true;
        }
        let (_hold, signal) = AbortHandle::new();
        self.pending_observers(&signal).await?;
        Ok(())
    }

    /// Forced drop/connection cancellation may discard undelivered publications.
    /// Normal lifecycle completion must call retire first, outside owner locks.
    pub(super) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.retiring = true;
        state.closed = true;
        for observer in state.observers.values() {
            observer.release.abort();
        }
        let observers = std::mem::take(&mut state.observers);
        let pending = std::mem::take(&mut state.pending);
        drop(state);
        drop(observers);
        drop(pending);
        self.progress.notify_waiters();
    }
}
