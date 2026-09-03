//! Internal hook-only plugin: construct a fresh instance for every review.
use std::sync::{Arc, Mutex};

use pi_core::{
    AgentPlugin, AgentPluginContext, PluginError, PluginId, ToolCallEvent, ToolCallPatch,
    ToolResultEvent, ToolResultPatch,
};

use crate::execution::{HermesRunLease, HermesRunState, HermesRuns};

pub(crate) struct HermesReviewPlugin {
    runs: Arc<HermesRuns>,
    state: Arc<HermesRunState>,
    binding: Mutex<Option<HermesRunLease>>,
}

impl HermesReviewPlugin {
    pub(crate) fn new(runs: Arc<HermesRuns>) -> Self {
        Self {
            runs,
            state: Arc::new(HermesRunState::review()),
            binding: Mutex::new(None),
        }
    }

    fn bind(&self, context: &AgentPluginContext) -> Result<(), PluginError> {
        let mut binding = self
            .binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lease) = &*binding {
            if &lease.run_id != context.run_id() {
                return Err(crate::hook_error(
                    self,
                    "tool_call",
                    "A Hermes review plugin cannot be reused across executions",
                ));
            }
        } else {
            *binding = Some(
                self.runs
                    .attach(context.run_id().clone(), Arc::clone(&self.state))
                    .map_err(|error| crate::hook_error(self, "tool_call", error))?,
            );
        }
        Ok(())
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for HermesReviewPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("memory-hermes-review")
    }

    async fn tool_call(
        &self,
        context: AgentPluginContext,
        _: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        self.bind(&context)?;
        Ok(ToolCallPatch::default())
    }

    async fn tool_result(
        &self,
        context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        // A failed/blocked read must never authorize a later skill mutation.
        if !event.result.is_error {
            self.bind(&context)?;
            self.state
                .review
                .as_ref()
                .expect("private review state")
                .observe_read(&event.tool_call.name, event.validated_args);
        }
        Ok(ToolResultPatch::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{AbortHandle, AgentHook, AgentHookInterests, RunId};

    #[test]
    fn review_plugin_only_intercepts_tools_and_cannot_be_reused_for_another_run() {
        let runs = Arc::new(HermesRuns::default());
        let plugin = HermesReviewPlugin::new(runs.clone());
        assert_eq!(
            plugin.hook_interests(),
            AgentHookInterests::from_hooks(&[AgentHook::ToolCall, AgentHook::ToolResult,])
        );
        let context = |run_id| {
            AgentPluginContext::unavailable_for_testing(
                plugin.id(),
                run_id,
                ".".into(),
                AbortHandle::new().1,
            )
        };
        let first = context(RunId::next());
        plugin.bind(&first).unwrap();
        plugin.bind(&first).unwrap();
        let second = context(RunId::next());
        assert!(plugin.bind(&second).is_err());
        assert!(runs.get(Some(second.run_id())).is_none());
        assert_eq!(runs.len(), 1);
        let weak = Arc::downgrade(&plugin.state);
        drop(plugin);
        assert_eq!(runs.len(), 0);
        assert!(weak.upgrade().is_none());
    }
}
