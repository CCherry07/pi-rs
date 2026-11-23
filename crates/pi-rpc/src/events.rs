//! Generation-bound synchronous projection into the connection's one ordered writer.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use pi_core::AbortSignal;
use pi_session::{
    AgentSession, AgentSessionEvent, RevisionedAgentSessionEvent, SessionEventObserver,
    SessionEventObserverGuard,
};
use serde_json::json;

use crate::json_wire::session_event_json;
use crate::output::RpcOutput;

pub(crate) struct RpcEventStream {
    output: RpcOutput,
    closed: Arc<AtomicBool>,
    subscriptions: Mutex<Vec<(Weak<AgentSession>, Option<SessionEventObserverGuard>)>>,
}

impl RpcEventStream {
    pub(crate) fn new(output: RpcOutput) -> Self {
        Self {
            output,
            closed: Arc::default(),
            subscriptions: Mutex::default(),
        }
    }

    /// Bind during generation preparation, before session_start can publish.
    /// Rebinding the same generation is harmless; closed generations are released.
    pub(crate) fn bind(&self, session: &Arc<AgentSession>) {
        let weak = Arc::downgrade(session);
        let mut removed = Vec::new();
        {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.closed.load(Ordering::Acquire) {
                return;
            }
            if subscriptions
                .iter()
                .any(|(current, _)| current.ptr_eq(&weak))
            {
                return;
            }
            subscriptions.retain_mut(|(current, guard)| {
                let live = current
                    .upgrade()
                    .is_some_and(|session| !session.is_closed());
                if !live {
                    removed.push(guard.take());
                }
                live
            });
            // Reserve before registration: a synchronous fallback notice may reenter bind.
            subscriptions.push((weak.clone(), None));
        }
        drop(removed);
        let observer = Arc::new(RpcSessionObserver {
            session: weak.clone(),
            output: self.output.clone(),
            closed: self.closed.clone(),
        });
        let mut guard = Some(session.observe_events(observer));
        {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((_, slot)) = subscriptions
                .iter_mut()
                .find(|(current, _)| current.ptr_eq(&weak))
            {
                *slot = guard.take();
            }
        }
        drop(guard);
    }

    pub(crate) async fn publication_barrier(
        &self,
        session: &Arc<AgentSession>,
    ) -> Result<(), String> {
        let barrier = {
            let subscriptions = self
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            subscriptions
                .iter()
                .find(|(current, _)| current.ptr_eq(&Arc::downgrade(session)))
                .and_then(|(_, guard)| guard.as_ref())
                .map(SessionEventObserverGuard::publication_barrier)
        };
        if let Some(barrier) = barrier {
            barrier.await?;
        }
        Ok(())
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let subscriptions = std::mem::take(
            &mut *self
                .subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        // Guard destruction may release an awaited producer. Never do it under our lock.
        drop(subscriptions);
    }
}

impl Drop for RpcEventStream {
    fn drop(&mut self) {
        self.close();
    }
}

struct RpcSessionObserver {
    session: Weak<AgentSession>,
    output: RpcOutput,
    closed: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl SessionEventObserver for RpcSessionObserver {
    fn on_event(&self, event: &RevisionedAgentSessionEvent) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(session) = self.session.upgrade() else {
            return Ok(());
        };
        let value = match &event.event {
            AgentSessionEvent::PluginNotice { message, level } => Some(json!({
                "type":"extension_ui_request", "id":uuid::Uuid::now_v7().to_string(),
                "method":"notify", "message":message, "notifyType":level,
            })),
            event => match session_event_json(event.clone(), &session) {
                Ok(value) => value,
                Err(error) => Some(json!({
                    "type":"extension_error", "extensionPath":"pi-rs/json-wire",
                    "event":"projection", "error":error,
                })),
            },
        };
        if let Some(value) = value {
            self.output.enqueue(value)?;
        }
        Ok(())
    }

    async fn flush(&self, _: AbortSignal) -> Result<(), String> {
        // This acknowledges the actual writer, not acceptance by an intermediate queue.
        self.output.flush().await
    }
}
