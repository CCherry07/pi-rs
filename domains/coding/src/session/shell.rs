use std::path::PathBuf;

use async_trait::async_trait;
use pi_session::SessionShellExecutor;
use pi_shell::{ShellError, ShellRequest, ShellResult};

/// Local Pi shell shorthand with generation-local settings.
#[derive(Debug, Default)]
pub struct CodingShellExecutor {
    shell_path: Option<PathBuf>,
    command_prefix: Option<String>,
}

impl CodingShellExecutor {
    pub fn new(shell_path: Option<PathBuf>, command_prefix: Option<String>) -> Self {
        Self {
            shell_path,
            command_prefix,
        }
    }
}

#[async_trait]
impl SessionShellExecutor for CodingShellExecutor {
    async fn execute(&self, mut request: ShellRequest) -> Result<ShellResult, ShellError> {
        if let Some(prefix) = self
            .command_prefix
            .as_deref()
            .filter(|prefix| !prefix.is_empty())
        {
            request.command = format!("{prefix}\n{}", request.command);
        }
        if request.shell_path.is_none() {
            request.shell_path.clone_from(&self.shell_path);
        }
        pi_shell::execute(request).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use pi_agent::AgentOptions;
    use pi_core::{ModelId, ProviderId, WorkspaceSpec};
    use pi_runtime::PiRuntime;
    use pi_session::{AgentSession, InitialModelRequest, ShellExecutionOptions};
    use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

    use super::*;

    fn runtime(cwd: &std::path::Path, turns: impl IntoIterator<Item = ScriptedTurn>) -> PiRuntime {
        PiRuntime::builder()
            .workspace(WorkspaceSpec::from_cwd(cwd))
            .provider_plugin(ScriptedProviderPlugin::scripted(turns))
            .agent_options(AgentOptions {
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
                ..AgentOptions::default()
            })
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn shell_settings_preserve_submitted_command_and_replay_never_executes_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let original = "printf x >> executions; printf '%s' \"$configured_value\"";
        let settings = pi_settings::SettingsValues {
            shell_path: Some("/missing-default-shell".to_string()),
            shell_command_prefix: Some("configured_value=from-settings".to_string()),
            ..pi_settings::SettingsValues::default()
        };
        let session = AgentSession::create_with_options(
            runtime(directory.path(), [ScriptedTurn::Text("done".to_string())]),
            &path,
            crate::configuration::session_options(&settings, InitialModelRequest::default()),
        )
        .await
        .unwrap();
        let error = session
            .execute_shell("printf ignored", ShellExecutionOptions::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("failed to spawn shell"));
        assert!(session.snapshot().bash.is_none());
        let result = session
            .execute_shell(
                original,
                ShellExecutionOptions {
                    shell_path: Some(PathBuf::from("/bin/sh")),
                    ..ShellExecutionOptions::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(result.output, "from-settings");
        assert!(
            !path.exists(),
            "shell-only use must not create a resume entry"
        );
        let message = session
            .log()
            .load()
            .unwrap()
            .messages()
            .into_iter()
            .find(|message| message.role() == "bashExecution")
            .unwrap();
        assert_eq!(message.as_custom().unwrap()["command"], original);

        session.prompt("hello").await.unwrap();
        session.shutdown().await;
        drop(session);
        let reopened = AgentSession::open(runtime(directory.path(), []), &path)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("executions")).unwrap(),
            "x"
        );
        assert!(
            reopened
                .log()
                .load()
                .unwrap()
                .messages()
                .iter()
                .any(|message| message.role() == "bashExecution")
        );
        reopened.shutdown().await;
    }
}
