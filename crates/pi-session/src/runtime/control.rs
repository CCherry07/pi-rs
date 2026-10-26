use super::AgentSession;
use crate::SessionError;
use pi_agent::{AgentLoopTurnUpdate, AgentTurnContext, AgentTurnControl, AgentTurnControlError};
use pi_core::StopReason;
use std::sync::{Arc, Weak};
struct SessionTurnControl {
    session: Weak<AgentSession>,
    previous: Arc<dyn AgentTurnControl>,
}

#[async_trait::async_trait]
impl AgentTurnControl for SessionTurnControl {
    async fn dispatch_turn_end(
        &self,
        run_id: pi_core::RunId,
        event: pi_plugin::TurnEndEvent,
        signal: pi_core::AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        let Some(session) = self.session.upgrade() else {
            return Ok(false);
        };
        let outcome = match event.message.stop_reason {
            StopReason::Aborted => pi_plugin::AgentActivityOutcome::Aborted,
            StopReason::Error => pi_plugin::AgentActivityOutcome::Error,
            _ => pi_plugin::AgentActivityOutcome::Completed,
        };
        let resume = session
            .run_boundary(&run_id, Some(event), outcome, &signal)
            .await
            .map_err(|error| AgentTurnControlError(error.to_string()))?;
        session.activity.request_continuation(resume);
        Ok(true)
    }
    async fn should_continue_after_turn(
        &self,
        turn: AgentTurnContext,
        signal: pi_core::AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        let requested = self
            .session
            .upgrade()
            .is_some_and(|session| session.activity.take_continuation());
        Ok(self
            .previous
            .should_continue_after_turn(turn, signal)
            .await?
            || requested)
    }

    async fn prepare_next_turn(
        &self,
        mut turn: AgentTurnContext,
        signal: pi_core::AbortSignal,
    ) -> Result<Option<AgentLoopTurnUpdate>, AgentTurnControlError> {
        let Some(session) = self.session.upgrade() else {
            return self.previous.prepare_next_turn(turn, signal).await;
        };
        let edited_context = session
            .context_updates
            .pending_context()
            .map_err(|error| AgentTurnControlError(error.to_string()))?
            .map(|projected| {
                let mut context = turn.context.as_ref().clone();
                context.messages = projected.runtime_messages();
                context
            });
        if let Some(context) = &edited_context {
            turn.context = Arc::new(context.clone());
        }
        let compacted_context = session
            .compact_before_next_turn(&turn)
            .await
            .or(edited_context);
        if let Some(context) = &compacted_context {
            turn.context = Arc::new(context.clone());
        }

        let previous = self.previous.prepare_next_turn(turn, signal).await?;
        match (previous, compacted_context) {
            (Some(update), _) if update.context.is_some() => Ok(Some(update)),
            (Some(mut update), Some(context)) => {
                update.context = Some(context);
                Ok(Some(update))
            }
            (Some(update), None) => Ok(Some(update)),
            (None, Some(context)) => Ok(Some(AgentLoopTurnUpdate {
                context: Some(context),
                ..AgentLoopTurnUpdate::default()
            })),
            (None, None) => Ok(None),
        }
    }

    async fn should_stop_after_turn(
        &self,
        context: AgentTurnContext,
        signal: pi_core::AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        let stopped = self
            .previous
            .should_stop_after_turn(context, signal)
            .await?;
        if stopped && let Some(session) = self.session.upgrade() {
            session.activity.stop();
        }
        Ok(stopped)
    }
}

impl AgentSession {
    pub(super) fn install_session_turn_control(session: &Arc<Self>) -> Result<(), SessionError> {
        let target = Arc::downgrade(session);
        session
            .runtime
            .agent()
            .compose_turn_control(move |previous| {
                Arc::new(SessionTurnControl {
                    session: target,
                    previous,
                })
            })
            .map_err(|error| SessionError::Runtime(error.to_string()))
    }
}
