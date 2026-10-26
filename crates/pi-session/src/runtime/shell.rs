use super::activity::ActivityPhase;
use super::context::ContextActivation;
use super::{AgentSession, ShellExecutionOptions};
use crate::{AgentMessage, SessionEntry, SessionError, next_unique_id, now_ms};
use pi_core::AbortHandle;
use pi_shell::{ShellChunk, ShellRequest, ShellResult};
use std::sync::Arc;

impl AgentSession {
    pub async fn execute_shell(
        &self,
        command: impl Into<String>,
        options: ShellExecutionOptions,
    ) -> Result<ShellResult, SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Shell).await;
        self.ensure_open()?;
        if self.activity.accepts_live_messages() {
            return Err(SessionError::Busy);
        }
        let executor = self.shell_executor.as_ref().ok_or_else(|| {
            SessionError::InvalidPayload(
                "shell execution is not configured for this session".to_string(),
            )
        })?;
        let command = command.into();
        if command.trim().is_empty() {
            return Err(SessionError::InvalidPayload(
                "shell command cannot be empty".to_string(),
            ));
        }
        let id = options.id.unwrap_or_else(|| next_unique_id("bash"));
        let (abort, signal) = AbortHandle::new();
        self.activity.register_shell(id.clone(), abort)?;
        self.events
            .publish_bash_start(id.clone(), command.clone(), options.exclude_from_context);
        let events = Arc::clone(&self.events);
        let update_id = id.clone();
        let execution = executor
            .execute(ShellRequest {
                command: command.clone(),
                cwd: self.runtime.cwd().to_path_buf(),
                timeout: options.timeout,
                shell_path: options.shell_path,
                abort_signal: signal,
                on_chunk: Some(Arc::new(move |chunk: ShellChunk| {
                    events.publish_bash_update(update_id.clone(), chunk.stream, chunk.text);
                })),
            })
            .await;
        self.activity.clear_shell(&id);
        let result = match execution {
            Ok(result) => result,
            Err(error) => {
                self.events
                    .publish_bash_end(id, None, Some(error.to_string()));
                return Err(SessionError::Runtime(error.to_string()));
            }
        };
        let message = AgentMessage::custom(serde_json::json!({
            "role": "bashExecution",
            "command": command,
            "output": result.output.clone(),
            "exitCode": result.exit_code,
            "cancelled": result.cancelled,
            "truncated": result.truncated,
            "fullOutputPath": result.full_output_path,
            "timestamp": now_ms(),
            "excludeFromContext": options.exclude_from_context,
        }))?;
        let entry = SessionEntry::message(message);
        if let Err(error) = self
            .context_updates
            .append(entry, ContextActivation::Restore)
        {
            self.events
                .publish_bash_end(id, Some(result), Some(error.to_string()));
            return Err(error);
        }
        self.events.publish_bash_end(id, Some(result.clone()), None);
        Ok(result)
    }

    pub fn abort_shell(&self) {
        self.activity.abort_shell();
    }
}
