//! Awaited product model mutations dispatch observers after committing the selected model.

use super::AgentSession;
use crate::{ModelCycleResult, SessionError};
use pi_core::{ModelId, ModelSpec, ProviderId, ThinkingLevel};
use pi_plugin::{ModelSelectEvent, ModelSelectSource, ThinkingLevelSelectEvent};

impl AgentSession {
    pub(super) fn emit_thinking_level_select(
        &self,
        level: ThinkingLevel,
        previous_level: ThinkingLevel,
    ) {
        let driver = std::sync::Arc::clone(&self.plugin_driver);
        let context = self.session_dispatch_context().clone();
        // Pi's thinking-level notification does not delay the command response or model selection.
        self.executor.spawn(async move {
            driver
                .thinking_level_select(
                    &context,
                    &ThinkingLevelSelectEvent {
                        level,
                        previous_level,
                    },
                )
                .await;
        });
    }

    pub async fn set_model_with_hooks(
        &self,
        provider: ProviderId,
        model_id: ModelId,
    ) -> Result<(), SessionError> {
        let previous = self.selected_model();
        self.set_model(provider, model_id)?;
        self.emit_model_select(previous, ModelSelectSource::Set)
            .await;
        Ok(())
    }

    pub async fn cycle_model_with_hooks(&self) -> Result<Option<ModelCycleResult>, SessionError> {
        let previous = self.selected_model();
        let result = self.cycle_model()?;
        if result.is_some() {
            self.emit_model_select(previous, ModelSelectSource::Cycle)
                .await;
        }
        Ok(result)
    }

    fn selected_model(&self) -> Option<ModelSpec> {
        let (provider, model_id) = self.runtime.agent().model_selection();
        self.runtime.model(&provider, &model_id)
    }

    async fn emit_model_select(&self, previous: Option<ModelSpec>, source: ModelSelectSource) {
        let Some(model) = self.selected_model() else {
            return;
        };
        if previous
            .as_ref()
            .is_some_and(|previous| previous.provider == model.provider && previous.id == model.id)
        {
            return;
        }
        self.plugin_driver()
            .model_select(
                self.session_dispatch_context(),
                &ModelSelectEvent {
                    model,
                    previous_model: previous,
                    source,
                },
            )
            .await;
    }
}
