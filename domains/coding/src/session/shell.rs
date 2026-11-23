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
        pi_shell::execute_user_shell(request).await
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
    async fn user_shell_sanitizes_streams_while_bash_tool_preserves_raw_output() {
        use pi_core::{AbortHandle, ContentBlock, ToolCallId};
        use pi_plugin::{Tool, ToolContext, ToolUpdateSink};
        use std::sync::{Arc, Mutex};

        let directory = tempfile::tempdir().unwrap();
        let command = "printf '\\033[31mred\\033[0m\\r\\n'";
        let (_, signal) = AbortHandle::new();
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let callback_chunks = Arc::clone(&chunks);
        let result = CodingShellExecutor::default()
            .execute(ShellRequest {
                command: command.into(),
                cwd: directory.path().into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal.clone(),
                on_chunk: Some(Arc::new(move |chunk| {
                    callback_chunks.lock().unwrap().push(chunk);
                })),
            })
            .await
            .unwrap();

        assert_eq!(result.output, "red\n");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.truncated);
        let streamed = chunks
            .lock()
            .unwrap()
            .iter()
            .map(|chunk| chunk.text.clone())
            .collect::<String>();
        assert_eq!(streamed, "red\n");

        let (updates, _) = ToolUpdateSink::channel();
        let tool_result = pi_plugin_bash::BashTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("raw-shell-tool"),
                serde_json::json!({"command": command}),
                updates,
            )
            .await
            .unwrap();
        let ContentBlock::Text(text) = &tool_result.content[0] else {
            panic!("expected tool text")
        };
        assert_eq!(text.text, "\x1b[31mred\x1b[0m\r\n");
        assert_eq!(
            tool_result.structured_content.as_ref().unwrap()["output"],
            "\x1b[31mred\x1b[0m\r\n"
        );
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
        let session = AgentSession::create(
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
            .history()
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
        let reopened = AgentSession::open(runtime(directory.path(), []), &path, Default::default())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.path().join("executions")).unwrap(),
            "x"
        );
        assert!(
            reopened
                .history()
                .load()
                .unwrap()
                .messages()
                .iter()
                .any(|message| message.role() == "bashExecution")
        );
        reopened.shutdown().await;
    }
}
