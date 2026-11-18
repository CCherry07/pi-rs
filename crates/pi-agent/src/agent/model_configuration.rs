use super::*;
use crate::{AgentLoopTurnUpdate, AgentTurnContext, AgentTurnControlError};
use pi_core::{AbortSignal, ModelSelection};

/// Reserves only model selection while the caller persists the change.
/// Message delivery and generation publication resume when this value is committed or dropped.
#[must_use = "commit the prepared model selection or drop it to leave the Agent unchanged"]
pub struct PreparedAgentModelConfiguration<'a> {
    state: std::sync::MutexGuard<'a, AgentState>,
    candidate: AgentStateSnapshot,
}

impl PreparedAgentModelConfiguration<'_> {
    pub fn configuration(&self) -> &AgentStateSnapshot {
        &self.candidate
    }

    pub fn commit(mut self) {
        self.state.snapshot.provider_id = self.candidate.provider_id;
        self.state.snapshot.model_id = self.candidate.model_id;
        self.state.snapshot.thinking_level = self.candidate.thinking_level;
        self.state.context_request = None;
    }
}

type Selection = (ProviderId, ModelId, ThinkingLevel);

fn selection(state: &AgentStateSnapshot) -> Selection {
    (
        state.provider_id.clone(),
        state.model_id.clone(),
        state.thinking_level,
    )
}

struct LiveModelTurnControl {
    inner: Weak<AgentInner>,
    previous: Arc<dyn AgentTurnControl>,
    applied: Mutex<Selection>,
}

#[async_trait::async_trait]
impl AgentTurnControl for LiveModelTurnControl {
    async fn dispatch_turn_end(
        &self,
        run_id: RunId,
        event: pi_plugin::TurnEndEvent,
        signal: AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        self.previous.dispatch_turn_end(run_id, event, signal).await
    }

    async fn should_continue_after_turn(
        &self,
        context: AgentTurnContext,
        signal: AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        self.previous
            .should_continue_after_turn(context, signal)
            .await
    }

    async fn should_stop_after_turn(
        &self,
        context: AgentTurnContext,
        signal: AbortSignal,
    ) -> Result<bool, AgentTurnControlError> {
        self.previous.should_stop_after_turn(context, signal).await
    }

    async fn prepare_next_turn(
        &self,
        context: AgentTurnContext,
        signal: AbortSignal,
    ) -> Result<Option<AgentLoopTurnUpdate>, AgentTurnControlError> {
        let mut update = self.previous.prepare_next_turn(context, signal).await?;
        let Some(inner) = self.inner.upgrade() else {
            return Ok(update);
        };
        let selected = selection(
            &inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .snapshot,
        );
        let mut applied = self
            .applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if selected != *applied {
            let update = update.get_or_insert_with(AgentLoopTurnUpdate::default);
            if selected.0 != applied.0 || selected.1 != applied.1 {
                update.provider_id = Some(selected.0.clone());
                update.model_id = Some(selected.1.clone());
            }
            if selected.2 != applied.2 {
                update.thinking_level = Some(selected.2);
            }
            *applied = selected;
        }
        Ok(update)
    }
}

impl Agent {
    /// Prepares a model/thinking change without interrupting an in-flight response.
    /// Subsequent provider requests use the committed selection in the same generation.
    pub fn prepare_model_configuration(
        &self,
        model: Option<ModelSelection>,
        thinking_level: Option<ThinkingLevel>,
    ) -> Result<PreparedAgentModelConfiguration<'_>, AgentError> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let runtime = self.runtime();
        if let Some(model) = &model
            && runtime.registries().provider(&model.provider).is_none()
        {
            return Err(AgentError::InvalidConfiguration(format!(
                "unknown provider: {}",
                model.provider
            )));
        }
        let mut candidate = state.snapshot.clone();
        if let Some(model) = model {
            candidate.provider_id = model.provider;
            candidate.model_id = model.model_id;
        }
        if let Some(thinking_level) = thinking_level {
            candidate.thinking_level = thinking_level;
        }
        Ok(PreparedAgentModelConfiguration { state, candidate })
    }

    pub(super) fn live_model_turn_control(
        &self,
        initial: &AgentStateSnapshot,
    ) -> Arc<dyn AgentTurnControl> {
        Arc::new(LiveModelTurnControl {
            inner: Arc::downgrade(&self.inner),
            previous: self.active_turn_control(),
            applied: Mutex::new(selection(initial)),
        })
    }
}
