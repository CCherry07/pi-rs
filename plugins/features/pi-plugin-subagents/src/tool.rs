use async_trait::async_trait;
use pi_core::{
    ContentBlock, CustomMessageContent, IsolatedSessionRequest, Message, TextContent, Tool,
    ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdate,
    ToolUpdateSink,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::catalog::SubagentCatalog;
use crate::launch_plan::SubagentLaunchPlan;
use crate::runtime::{LaunchTicket, SubagentRuntime};

const MAX_TASK_BYTES: usize = 64 * 1024;

pub(crate) struct SubagentTool {
    runtime: SubagentRuntime,
    catalog: SubagentCatalog,
    max_depth: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubagentInput {
    agent: String,
    task: String,
}

struct RunGuard {
    runtime: SubagentRuntime,
    run_id: String,
    launched: bool,
}

impl RunGuard {
    fn reserved(runtime: SubagentRuntime, ticket: &LaunchTicket) -> Self {
        Self {
            runtime,
            run_id: ticket.run_id().to_string(),
            launched: false,
        }
    }

    fn mark_launched(&mut self) {
        self.launched = true;
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.launched {
            self.runtime.finish(&self.run_id);
        } else {
            self.runtime.cancel_unlaunched(&self.run_id);
        }
    }
}

impl SubagentTool {
    pub(crate) fn new(
        runtime: SubagentRuntime,
        catalog: SubagentCatalog,
        max_depth: usize,
    ) -> Self {
        Self {
            runtime,
            catalog,
            max_depth,
        }
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "subagent".to_string(),
            label: "Delegate task".to_string(),
            description: "Delegate one focused task to an isolated child Pi session and return its final response. Multiple independent calls in one assistant turn can run in parallel.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "enum": self.catalog.profile_names(),
                        "description": "Focused child role"
                    },
                    "task": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Self-contained task for the child session"
                    }
                },
                "required": ["agent", "task"],
                "additionalProperties": false
            }),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: Some(format!(
                "Delegate focused work to an isolated child session. Available roles:\n{}",
                self.catalog.formatted_catalog()
            )),
            prompt_guidelines: vec![
                "Give the child a self-contained task with the relevant goal, constraints, and paths."
                    .to_string(),
                "Use separate subagent calls for independent work; calls emitted together execute in parallel."
                    .to_string(),
                "Keep one `worker` as the writer for overlapping files; parallelize `scout`, `reviewer`, and `oracle` work instead."
                    .to_string(),
                "Children may delegate recursively, but the feature runtime enforces depth, cumulative spawn, and active-run limits."
                    .to_string(),
            ],
        }
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        parse_input(input.clone(), &self.catalog).map(|_| ())
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let input = parse_input(input, &self.catalog)?;
        let profile = self
            .catalog
            .profile(&input.agent)
            .expect("validated profile must exist");
        let launch_plan = SubagentLaunchPlan::resolve(&profile, &context)?;
        let profile_name = profile.name.clone();
        let timeout = profile.timeout;
        let timeout_ms =
            timeout.map(|timeout| u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        let parent_session_id = context.session.id()?;
        let ticket = self
            .runtime
            .begin_launch_with_max_depth(&parent_session_id, profile, self.max_depth)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let mut guard = RunGuard::reserved(self.runtime.clone(), &ticket);
        let run_id = ticket.run_id().to_string();
        let depth = ticket.depth();
        let request = IsolatedSessionRequest::new(CustomMessageContent::Text(
            ticket.child_prompt(&input.task),
        ))
        .options(launch_plan.into_options());
        let handle = match context.session.launch_isolated_session(request).await {
            Ok(handle) => handle,
            Err(error) => return Err(error.into()),
        };
        guard.mark_launched();
        updates.send(ToolUpdate {
            content: vec![ContentBlock::Text(TextContent::new(format!(
                "{} subagent running",
                profile_name
            )))],
            details: Some(json!({
                "runId": run_id,
                "agent": profile_name,
                "depth": depth,
                "state": "running"
            })),
        });

        let deadline = async {
            match timeout {
                Some(timeout) => tokio::time::sleep(timeout).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(deadline);
        let outcome = tokio::select! {
            outcome = handle.wait() => outcome?,
            () = context.signal().wait() => {
                let _ = handle.abort();
                let _ = handle.wait().await;
                return Err(ToolError::Aborted);
            }
            () = &mut deadline => {
                let _ = handle.abort();
                let warnings = self.runtime.warnings(&run_id);
                let mut result = ToolResult::error(with_warnings(
                    format!(
                        "{} subagent timed out after {} ms",
                        profile_name,
                        timeout_ms.expect("deadline exists only for configured timeouts")
                    ),
                    &warnings,
                ));
                result.details = Some(json!({
                    "runId": run_id,
                    "isolatedSessionId": handle.id().as_str(),
                    "agent": profile_name,
                    "depth": depth,
                    "state": "timed_out",
                    "timeoutMs": timeout_ms.expect("deadline exists only for configured timeouts"),
                    "warnings": warnings,
                }));
                return Ok(result);
            }
        };
        let warnings = self.runtime.warnings(&run_id);
        let text = with_warnings(final_text(&outcome.messages), &warnings);
        let details = json!({
            "runId": run_id,
            "isolatedSessionId": handle.id().as_str(),
            "sessionId": outcome.session_id,
            "agent": profile_name,
            "depth": depth,
            "state": if outcome.aborted { "aborted" } else { "completed" },
            "aborted": outcome.aborted,
            "warnings": warnings,
        });
        if outcome.aborted {
            let mut result = ToolResult::error(format!(
                "{} subagent was aborted before it completed",
                profile_name
            ));
            result.details = Some(details);
            return Ok(result);
        }
        let mut result = ToolResult::text(text);
        result.details = Some(details);
        Ok(result)
    }
}

fn parse_input(input: Value, catalog: &SubagentCatalog) -> Result<SubagentInput, ToolError> {
    let parsed: SubagentInput = serde_json::from_value(input)
        .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
    let agent = parsed.agent.trim();
    if catalog.profile(agent).is_none() {
        return Err(ToolError::InvalidArguments(format!(
            "unknown subagent profile '{}'; expected one of: {}",
            parsed.agent,
            catalog.profile_names().join(", ")
        )));
    }
    let task = parsed.task.trim();
    if task.is_empty() {
        return Err(ToolError::InvalidArguments(
            "task must not be empty".to_string(),
        ));
    }
    if task.len() > MAX_TASK_BYTES {
        return Err(ToolError::InvalidArguments(format!(
            "task exceeds the {MAX_TASK_BYTES}-byte limit"
        )));
    }
    Ok(SubagentInput {
        agent: agent.to_string(),
        task: task.to_string(),
    })
}

fn final_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .filter_map(|message| match message {
            Message::Assistant(message) => Some(message),
            _ => None,
        })
        .find_map(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|content| match content {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        })
        .unwrap_or_else(|| "Subagent completed without a textual response.".to_string())
}

fn with_warnings(mut text: String, warnings: &[String]) -> String {
    if warnings.is_empty() {
        return text;
    }
    text.push_str("\n\nSubagent warnings:\n");
    for warning in warnings {
        text.push_str("- ");
        text.push_str(warning);
        text.push('\n');
    }
    text.pop();
    text
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use pi_core::{
        AbortHandle, AssistantMessage, IsolatedSessionId, IsolatedSessionOutcome, ModelId,
        ModelSpec, ModelsContextAccess, PluginContext, PluginContextEpoch, PluginContextResult,
        PluginContextScope, ProviderId, SessionContextAccess, StopReason, ThinkingLevel,
        UiContextAccess, Usage,
    };

    use super::*;
    use crate::runtime::run_marker;
    use tokio::sync::Notify;

    #[test]
    fn input_is_strict_and_normalized() {
        let catalog = SubagentCatalog::builtins();
        let parsed = parse_input(
            json!({"agent": " reviewer ", "task": " inspect "}),
            &catalog,
        )
        .unwrap();
        assert_eq!(parsed.agent, "reviewer");
        assert_eq!(parsed.task, "inspect");
        assert!(parse_input(json!({"agent": "unknown", "task": "inspect"}), &catalog).is_err());
        assert!(
            parse_input(
                json!({"agent": "scout", "task": "", "extra": true}),
                &catalog
            )
            .is_err()
        );
    }

    #[test]
    fn result_projection_uses_the_last_textual_assistant_message() {
        let assistant = |text: &str| {
            Message::Assistant(Arc::new(AssistantMessage {
                content: vec![ContentBlock::Text(TextContent::new(text))],
                api: "test".to_string(),
                provider: ProviderId::new("test"),
                model: ModelId::new("test"),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                deferred: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp_ms: 0,
            }))
        };
        assert_eq!(
            final_text(&[assistant("first"), assistant("final")]),
            "final"
        );
        assert_eq!(
            final_text(&[]),
            "Subagent completed without a textual response."
        );
    }

    struct FakeAccess {
        requests: Mutex<Vec<IsolatedSessionRequest>>,
        outcome: IsolatedSessionOutcome,
    }

    #[async_trait]
    impl SessionContextAccess for FakeAccess {
        fn session_id(&self) -> PluginContextResult<String> {
            Ok("root-session".to_string())
        }

        fn active_tools(&self) -> PluginContextResult<Vec<String>> {
            Ok(["read", "grep", "subagent"].map(str::to_string).to_vec())
        }

        async fn launch_isolated_session(
            &self,
            _scope: PluginContextScope,
            request: IsolatedSessionRequest,
        ) -> PluginContextResult<IsolatedSessionId> {
            self.requests.lock().unwrap().push(request);
            Ok(IsolatedSessionId::new("isolated-1"))
        }

        async fn wait_for_isolated_session(
            &self,
            _scope: PluginContextScope,
            _id: IsolatedSessionId,
        ) -> PluginContextResult<IsolatedSessionOutcome> {
            Ok(self.outcome.clone())
        }
    }

    #[async_trait]
    impl ModelsContextAccess for FakeAccess {
        fn model_selection(&self) -> PluginContextResult<Option<pi_core::ModelSelection>> {
            Ok(Some(pi_core::ModelSelection::new("scripted", "parent")))
        }

        fn model(&self) -> PluginContextResult<Option<ModelSpec>> {
            Ok(Some(reasoning_model("parent")))
        }

        fn available_models(&self) -> PluginContextResult<Vec<ModelSpec>> {
            Ok(vec![reasoning_model("parent"), reasoning_model("child")])
        }
    }

    #[async_trait]
    impl UiContextAccess for FakeAccess {}

    struct TimeoutAccess {
        requests: Mutex<Vec<IsolatedSessionRequest>>,
        aborted: AtomicBool,
        wake: Notify,
    }

    #[async_trait]
    impl SessionContextAccess for TimeoutAccess {
        fn session_id(&self) -> PluginContextResult<String> {
            Ok("root-session".to_string())
        }

        fn active_tools(&self) -> PluginContextResult<Vec<String>> {
            Ok(["read", "subagent"].map(str::to_string).to_vec())
        }

        async fn launch_isolated_session(
            &self,
            _scope: PluginContextScope,
            request: IsolatedSessionRequest,
        ) -> PluginContextResult<IsolatedSessionId> {
            self.requests.lock().unwrap().push(request);
            Ok(IsolatedSessionId::new("isolated-timeout"))
        }

        async fn wait_for_isolated_session(
            &self,
            _scope: PluginContextScope,
            _id: IsolatedSessionId,
        ) -> PluginContextResult<IsolatedSessionOutcome> {
            if !self.aborted.load(Ordering::SeqCst) {
                self.wake.notified().await;
            }
            Ok(IsolatedSessionOutcome {
                session_id: "timed-out-child".to_string(),
                messages: Vec::new(),
                aborted: true,
            })
        }

        fn abort_isolated_session(
            &self,
            _scope: PluginContextScope,
            _id: IsolatedSessionId,
        ) -> PluginContextResult<()> {
            self.aborted.store(true, Ordering::SeqCst);
            self.wake.notify_waiters();
            Ok(())
        }
    }

    #[async_trait]
    impl ModelsContextAccess for TimeoutAccess {
        fn model_selection(&self) -> PluginContextResult<Option<pi_core::ModelSelection>> {
            Ok(Some(pi_core::ModelSelection::new("scripted", "parent")))
        }

        fn model(&self) -> PluginContextResult<Option<ModelSpec>> {
            Ok(Some(reasoning_model("parent")))
        }

        fn available_models(&self) -> PluginContextResult<Vec<ModelSpec>> {
            Ok(vec![reasoning_model("parent")])
        }
    }

    #[async_trait]
    impl UiContextAccess for TimeoutAccess {}

    #[tokio::test]
    async fn tool_launches_a_fresh_child_and_projects_its_final_answer() {
        let outcome = IsolatedSessionOutcome {
            session_id: "child-session".to_string(),
            messages: vec![Message::Assistant(Arc::new(AssistantMessage {
                content: vec![ContentBlock::Text(TextContent::new("review complete"))],
                api: "test".to_string(),
                provider: ProviderId::new("test"),
                model: ModelId::new("test"),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                deferred: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp_ms: 0,
            }))],
            aborted: false,
        };
        let access = Arc::new(FakeAccess {
            requests: Mutex::new(Vec::new()),
            outcome,
        });
        let plugin_access: Arc<dyn PluginContext> = access.clone();
        let epoch = PluginContextEpoch::new(plugin_access);
        let (_abort, signal) = AbortHandle::new();
        let context =
            ToolContext::with_plugin_context(PathBuf::from("/workspace"), signal, epoch.context());
        let (updates, mut update_receiver) = ToolUpdateSink::channel();

        let result = SubagentTool::new(
            SubagentRuntime::default(),
            SubagentCatalog::builtins(),
            crate::runtime::DEFAULT_MAX_DEPTH,
        )
        .execute(
            context,
            ToolCallId::new("call-1"),
            json!({"agent": "reviewer", "task": "Review the parser"}),
            updates,
        )
        .await
        .unwrap();

        assert_eq!(
            result.content,
            vec![ContentBlock::Text(TextContent::new("review complete"))]
        );
        assert_eq!(result.details.as_ref().unwrap()["agent"], "reviewer");
        assert_eq!(result.details.as_ref().unwrap()["depth"], 1);
        assert_eq!(
            update_receiver.recv().await.unwrap().details.unwrap()["state"],
            "running"
        );
        let requests = access.requests.lock().unwrap();
        let CustomMessageContent::Text(prompt) = &requests[0].input else {
            panic!("expected text child prompt");
        };
        assert!(run_marker(prompt).is_some());
        assert!(prompt.contains("`reviewer` delegated subagent"));
        assert!(prompt.contains("Review the parser"));
        assert_eq!(
            requests[0].options.active_tools,
            Some(["read", "grep", "subagent"].map(str::to_string).to_vec())
        );
    }

    #[tokio::test]
    async fn configured_zero_depth_blocks_before_creating_a_child_session() {
        let access = Arc::new(FakeAccess {
            requests: Mutex::new(Vec::new()),
            outcome: IsolatedSessionOutcome {
                session_id: "unused-child".to_string(),
                messages: Vec::new(),
                aborted: false,
            },
        });
        let plugin_access: Arc<dyn PluginContext> = access.clone();
        let epoch = PluginContextEpoch::new(plugin_access);
        let (_abort, signal) = AbortHandle::new();
        let context =
            ToolContext::with_plugin_context(PathBuf::from("/workspace"), signal, epoch.context());
        let (updates, _receiver) = ToolUpdateSink::channel();

        let error = SubagentTool::new(SubagentRuntime::default(), SubagentCatalog::builtins(), 0)
            .execute(
                context,
                ToolCallId::new("call-blocked"),
                json!({"agent": "reviewer", "task": "Review the parser"}),
                updates,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("nesting limit reached"));
        assert!(access.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn markdown_runtime_selection_is_resolved_into_the_isolated_request() {
        let directory = tempfile::tempdir().unwrap();
        let definitions = directory.path().join("agents");
        std::fs::create_dir_all(&definitions).unwrap();
        std::fs::write(
            definitions.join("configured.md"),
            "---\nname: configured\ndescription: Configured child\ntools: read, grep\nmodel: child\nthinking: high\nallowNestedSubagents: false\n---\nInspect the task.",
        )
        .unwrap();
        let mut loader =
            crate::catalog::SubagentLoaderOptions::new(directory.path(), directory.path());
        loader.additional_paths.push(definitions);
        let catalog = SubagentCatalog::load(&loader).unwrap();
        let access = Arc::new(FakeAccess {
            requests: Mutex::new(Vec::new()),
            outcome: IsolatedSessionOutcome {
                session_id: "child-session".to_string(),
                messages: Vec::new(),
                aborted: false,
            },
        });
        let plugin_access: Arc<dyn PluginContext> = access.clone();
        let epoch = PluginContextEpoch::new(plugin_access);
        let (_abort, signal) = AbortHandle::new();
        let context =
            ToolContext::with_plugin_context(PathBuf::from("/workspace"), signal, epoch.context());
        let (updates, _receiver) = ToolUpdateSink::channel();

        SubagentTool::new(
            SubagentRuntime::default(),
            catalog,
            crate::runtime::DEFAULT_MAX_DEPTH,
        )
        .execute(
            context,
            ToolCallId::new("call-configured"),
            json!({"agent": "configured", "task": "Inspect"}),
            updates,
        )
        .await
        .unwrap();

        let requests = access.requests.lock().unwrap();
        assert_eq!(
            requests[0].options.active_tools,
            Some(vec!["read".to_string(), "grep".to_string()])
        );
        assert_eq!(
            requests[0].options.model,
            Some(pi_core::ModelSelection::new("scripted", "child"))
        );
        assert_eq!(
            requests[0].options.thinking_level,
            Some(ThinkingLevel::High)
        );
    }

    #[tokio::test]
    async fn configured_timeout_aborts_the_child_and_returns_a_terminal_tool_result() {
        let directory = tempfile::tempdir().unwrap();
        let definitions = directory.path().join("agents");
        std::fs::create_dir_all(&definitions).unwrap();
        std::fs::write(
            definitions.join("timed.md"),
            "---\nname: timed\ndescription: Timed child\ntimeoutMs: 5\n---\nWait.",
        )
        .unwrap();
        let mut loader =
            crate::catalog::SubagentLoaderOptions::new(directory.path(), directory.path());
        loader.additional_paths.push(definitions);
        let catalog = SubagentCatalog::load(&loader).unwrap();
        let access = Arc::new(TimeoutAccess {
            requests: Mutex::new(Vec::new()),
            aborted: AtomicBool::new(false),
            wake: Notify::new(),
        });
        let plugin_access: Arc<dyn PluginContext> = access.clone();
        let epoch = PluginContextEpoch::new(plugin_access);
        let (_abort, signal) = AbortHandle::new();
        let context =
            ToolContext::with_plugin_context(PathBuf::from("/workspace"), signal, epoch.context());
        let (updates, _receiver) = ToolUpdateSink::channel();

        let result = SubagentTool::new(
            SubagentRuntime::default(),
            catalog,
            crate::runtime::DEFAULT_MAX_DEPTH,
        )
        .execute(
            context,
            ToolCallId::new("call-timeout"),
            json!({"agent": "timed", "task": "Wait forever"}),
            updates,
        )
        .await
        .unwrap();

        assert!(result.is_error);
        assert_eq!(result.details.as_ref().unwrap()["state"], "timed_out");
        assert_eq!(result.details.as_ref().unwrap()["timeoutMs"], 5);
        assert!(access.aborted.load(Ordering::SeqCst));
    }

    fn reasoning_model(id: &str) -> ModelSpec {
        let mut model = ModelSpec::new("scripted", id, id, "scripted");
        model.reasoning = true;
        model
    }
}
