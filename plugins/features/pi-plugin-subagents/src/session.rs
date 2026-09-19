use crate::{SubagentRuntime, SubagentsPlugin};
use pi_plugin::{Plugin, PluginId, SessionPluginContext, SessionShutdownEvent};
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pi_session::{SessionIdentity, SessionShutdownReason};

    use super::*;
    use crate::profiles::builtin_profile;
    use crate::runtime::LaunchError;

    #[tokio::test]
    async fn shutdown_releases_the_owner_budget() {
        let runtime = SubagentRuntime::without_persistence_for_testing();
        let plugin = SubagentsPlugin::new(runtime.clone());
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
            .begin_launch("root", builtin_profile("delegate"), 4, false)
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
