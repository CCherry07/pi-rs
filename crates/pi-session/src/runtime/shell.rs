use super::activity::ActivityPhase;
use super::context::ContextActivation;
use super::{AgentSession, ShellExecutionOptions};
use crate::{AgentMessage, SessionEntry, SessionError, next_unique_id, now_ms};
use pi_core::AbortHandle;
use pi_plugin::{UserBashEvent, UserBashOperations, UserBashOverride, UserBashRequest};
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
        let command = command.into();
        if command.trim().is_empty() {
            return Err(SessionError::InvalidPayload(
                "shell command cannot be empty".to_string(),
            ));
        }
        let request_id = options.id.clone();
        let id = options.id.unwrap_or_else(|| next_unique_id("bash"));
        let (abort, signal) = AbortHandle::new();
        self.activity.register_shell(id.clone(), abort)?;
        self.events
            .publish_bash_start(id.clone(), command.clone(), options.exclude_from_context);
        let events = Arc::clone(&self.events);
        let update_id = id.clone();
        let execution = async {
            let replacement = self
                .plugin_driver()
                .user_bash(
                    self.session_dispatch_context(),
                    &UserBashEvent {
                        command: command.clone(),
                        exclude_from_context: options.exclude_from_context,
                        cwd: self.runtime.cwd().to_path_buf(),
                        signal: signal.clone(),
                    },
                )
                .await?;
            if let Some(UserBashOverride::Result(result)) = replacement {
                return Ok(ShellResult {
                    output: result.output,
                    exit_code: result.exit_code,
                    cancelled: result.cancelled,
                    timed_out: false,
                    shell_exit_after: None,
                    truncated: result.truncated,
                    truncation: None,
                    full_output_path: result.full_output_path,
                });
            }
            let request = ShellRequest {
                command: command.clone(),
                cwd: self.runtime.cwd().to_path_buf(),
                timeout: options.timeout,
                shell_path: options.shell_path,
                abort_signal: signal,
                on_chunk: Some(Arc::new(move |chunk: ShellChunk| {
                    events.publish_bash_update(
                        update_id.clone(),
                        request_id.clone(),
                        chunk.stream,
                        chunk.text,
                    );
                })),
            };
            if let Some(UserBashOverride::Operations(operations)) = replacement {
                return pi_shell::execute_with_operations(
                    request,
                    &PluginShellOperations(operations),
                )
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()));
            }
            let executor = self.shell_executor.as_ref().ok_or_else(|| {
                SessionError::InvalidPayload(
                    "shell execution is not configured for this session".to_string(),
                )
            })?;
            executor
                .execute(request)
                .await
                .map_err(|error| SessionError::Runtime(error.to_string()))
        }
        .await;
        self.activity.clear_shell(&id);
        let result = match execution {
            Ok(result) => result,
            Err(error) => {
                self.events
                    .publish_bash_end(id, None, Some(error.to_string()));
                return Err(error);
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

struct PluginShellOperations(Arc<dyn UserBashOperations>);

#[async_trait::async_trait]
impl pi_shell::ShellOperations for PluginShellOperations {
    async fn execute(
        &self,
        request: pi_shell::ShellOperationsRequest,
    ) -> Result<Option<i32>, pi_shell::ShellError> {
        self.0
            .execute(UserBashRequest {
                command: request.command,
                cwd: request.cwd,
                signal: request.signal,
                on_data: request.on_data,
            })
            .await
            .map_err(|error| pi_shell::ShellError::Process(error.to_string()))
    }
}
