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
        // Quote values so directory names and paths remain on one line even with newlines.
        let cwd = serde_json::to_string(context.workspace().cwd())
            .map_err(|error| PluginError::Failure(error.to_string()))?;
        let roots = context
            .workspace()
            .roots()
            .iter()
            .map(|root| {
                Ok(format!(
                    "- {}: {}",
                    serde_json::to_string(&root.name)?,
                    serde_json::to_string(&root.path)?
                ))
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()
            .map_err(|error| PluginError::Failure(error.to_string()))?
            .join("\n");
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(format!(
                "{}\n\nWorking directory: {}\nWorkspace roots:\n{}\n\nRelative paths resolve from the working directory.",
                event.system_prompt, cwd, roots
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
        let mut primary = pi_core::WorkspaceRoot::external("internal-app-id", "app", "/app");
        primary.ownership = pi_core::WorkspaceRootOwnership::ManagedWorktree {
            source_root: "/source-repository".into(),
        };
        let spec = pi_core::WorkspaceSpec::new(
            vec![
                primary,
                pi_core::WorkspaceRoot::external("internal-shared-id", "shared\nfiles", "/shared"),
            ],
            pi_core::WorkspaceRootId::new("internal-app-id"),
            "/app/src",
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
            assert_eq!(
                prompt,
                "base\n\nWorking directory: \"/app/src\"\nWorkspace roots:\n- \"app\": \"/app\"\n- \"shared\\nfiles\": \"/shared\"\n\nRelative paths resolve from the working directory."
            );
        }
        assert_eq!(event.system_prompt, "base");
    }
}
