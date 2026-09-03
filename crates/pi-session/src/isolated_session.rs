use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pi_agent::AgentLoopStop;
use pi_core::{
    CustomMessageContent, IsolatedSessionId, IsolatedSessionOutcome, Message, UserMessage,
};
use tokio::sync::watch;

use crate::{PiSession, now_ms};

type IsolatedResult = Result<IsolatedSessionOutcome, String>;

pub(crate) struct IsolatedSessionRegistry {
    runs: Mutex<HashMap<IsolatedSessionId, Arc<IsolatedSessionRun>>>,
}

struct IsolatedSessionRun {
    owner_registration_id: String,
    session: PiSession,
    result: watch::Receiver<Option<IsolatedResult>>,
}

impl Default for IsolatedSessionRegistry {
    fn default() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
        }
    }
}

impl IsolatedSessionRegistry {
    pub(crate) async fn launch(
        &self,
        owner_registration_id: String,
        session: PiSession,
        input: CustomMessageContent,
    ) -> IsolatedSessionId {
        let id = IsolatedSessionId::new(session.registration_id().to_owned());
        let session_id = session.id();
        let prompt_session = session.current();
        let running_session = Arc::clone(&prompt_session);
        let (result_sender, result) = watch::channel(None);
        let readiness = result.clone();
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id.clone(),
                Arc::new(IsolatedSessionRun {
                    owner_registration_id,
                    session,
                    result,
                }),
            );
        tokio::spawn(async move {
            let message = Message::User(UserMessage {
                content: input.to_blocks(),
                timestamp_ms: now_ms(),
            });
            let result = prompt_session
                .prompt(vec![message])
                .await
                .map(|outcome| IsolatedSessionOutcome {
                    session_id,
                    messages: outcome.new_messages,
                    aborted: outcome.stop == AgentLoopStop::Aborted,
                })
                .map_err(|error| error.to_string());
            result_sender.send_replace(Some(result));
        });
        while !running_session.runtime().agent().is_running() && readiness.borrow().is_none() {
            if readiness.has_changed().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
        id
    }

    pub(crate) async fn wait(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<IsolatedSessionOutcome, String> {
        let run = self.owned_run(owner_registration_id, id)?;
        let mut result = run.result.clone();
        loop {
            if let Some(outcome) = result.borrow().clone() {
                return outcome;
            }
            result.changed().await.map_err(|_| {
                format!(
                    "isolated session {} ended without a terminal outcome",
                    id.as_str()
                )
            })?;
        }
    }

    pub(crate) fn abort(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<(), String> {
        self.owned_run(owner_registration_id, id)?.session.abort();
        Ok(())
    }

    pub(crate) fn remove_owned(&self, owner_registration_id: &str) -> Vec<PiSession> {
        let mut runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owned = runs
            .iter()
            .filter(|(_, run)| run.owner_registration_id == owner_registration_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        owned
            .into_iter()
            .filter_map(|id| runs.remove(&id))
            .map(|run| run.session.clone())
            .collect()
    }

    pub(crate) fn remove_session(&self, registration_id: &str) {
        let mut runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = runs.iter().find_map(|(id, run)| {
            (run.session.registration_id() == registration_id).then(|| id.clone())
        });
        if let Some(id) = id {
            runs.remove(&id);
        }
    }

    fn owned_run(
        &self,
        owner_registration_id: &str,
        id: &IsolatedSessionId,
    ) -> Result<Arc<IsolatedSessionRun>, String> {
        let run = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown isolated session: {}", id.as_str()))?;
        if run.owner_registration_id != owner_registration_id {
            return Err(format!(
                "isolated session {} is not owned by the current session",
                id.as_str()
            ));
        }
        Ok(run)
    }
}
