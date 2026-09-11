use async_trait::async_trait;
use pi_core::PluginId;
use pi_session::{SessionPlugin, SessionPluginContext, SessionPluginError, SessionShutdownEvent};

use crate::SubagentRuntime;

/// Session-lifecycle half of the feature. Agent/tool orchestration and
/// session cleanup remain distinct plugin systems while sharing one runtime.
pub struct SubagentsSessionPlugin {
    runtime: SubagentRuntime,
}

impl SubagentsSessionPlugin {
    pub fn new(runtime: SubagentRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl SessionPlugin for SubagentsSessionPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("subagents")
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        _event: &pi_session::SessionStartEvent,
    ) -> Result<(), SessionPluginError> {
        self.runtime.bind_session(
            context.identity().id.clone(),
            context.plugin_context_handle(),
        );
        Ok(())
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> Result<(), SessionPluginError> {
        self.runtime.close_owner(&context.identity().id);
        self.runtime.drain_monitors(&context.identity().id).await;
        self.runtime.forget_session(&context.identity().id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pi_session::{SessionIdentity, SessionShutdownReason};

    use super::*;
    use crate::profiles::builtin_profile;
    use crate::runtime::LaunchError;

    #[tokio::test]
    async fn shutdown_releases_the_owner_budget() {
        let runtime = SubagentRuntime::default();
        let plugin = SubagentsSessionPlugin::new(runtime.clone());
        let context = SessionPluginContext::unavailable_for_testing(
            PluginId::new("subagents"),
            1,
            SessionIdentity {
                id: "root".to_string(),
                path: PathBuf::from("session.jsonl"),
                cwd: PathBuf::from("."),
                parent_session_id: None,
            },
        );
        let run = runtime
            .begin_launch("root", builtin_profile("delegate"), 4)
            .unwrap();

        plugin
            .session_shutdown(
                &context,
                &SessionShutdownEvent {
                    reason: SessionShutdownReason::Quit,
                    target_session_file: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            runtime.bind_child(run.id(), "other").unwrap_err(),
            LaunchError::UnknownAgent(run.id().to_string())
        );
    }
}
