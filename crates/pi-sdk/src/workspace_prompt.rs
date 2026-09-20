use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, Plugin, PluginError, PluginId,
};

pub(crate) struct WorkspacePromptPlugin;

#[pi_plugin::plugin]
impl Plugin for WorkspacePromptPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("pi.workspace")
    }

    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        if context.workspace().roots().len() < 2 {
            return Ok(BeforeAgentStartPatch::default());
        }
        // JSON quotes directory names so they remain data even with newlines.
        let description = serde_json::to_string_pretty(context.workspace().spec())
            .map_err(|error| PluginError::Failure(error.to_string()))?;
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(format!(
                "{}\n\nWorkspace directories (environment data):\n{}\nRelative paths resolve from executionDir. Use explicit paths for supplemental roots. Resources are discovered from executionDir. These roots do not restrict filesystem access.",
                event.system_prompt, description
            )),
            ..BeforeAgentStartPatch::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn roots_are_described_per_run_without_accumulating_in_base_prompt() {
        let spec = pi_core::WorkspaceSpec::new(
            vec![
                pi_core::WorkspaceRoot::external("app", "app", "/app"),
                pi_core::WorkspaceRoot::external("shared", "shared", "/shared"),
            ],
            pi_core::WorkspaceRootId::new("app"),
            "/app",
        )
        .unwrap();
        let epoch = pi_plugin::PluginContextEpoch::with_workspace(
            Arc::new(pi_plugin::UnavailablePluginContext),
            Some(spec.snapshot()),
        );
        let driver =
            pi_plugin::PluginDriver::new_with_context(vec![Arc::new(WorkspacePromptPlugin)], epoch)
                .unwrap();
        let event = BeforeAgentStartEvent {
            system_prompt: "base".into(),
            input_messages: vec![],
            active_tools: vec![],
            provider_id: pi_core::ProviderId::new("scripted"),
            model_id: pi_core::ModelId::new("test"),
        };
        for _ in 0..2 {
            let result = driver
                .before_agent_start(
                    &pi_core::RunId::next(),
                    spec.cwd(),
                    &pi_core::AbortHandle::new().1,
                    event.clone(),
                )
                .await
                .unwrap();
            let prompt = result.system_prompt.unwrap();
            assert!(prompt.contains("/shared"));
            assert_eq!(prompt.matches("Workspace directories").count(), 1);
        }
        assert_eq!(event.system_prompt, "base");
    }
}
