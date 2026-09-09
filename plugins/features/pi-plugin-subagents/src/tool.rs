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
    context: Option<pi_core::IsolatedContextMode>,
}

struct RunGuard {
    runtime: SubagentRuntime,
    run_id: String,
    transferred: bool,
}

impl RunGuard {
    fn reserved(runtime: SubagentRuntime, ticket: &LaunchTicket) -> Self {
        Self {
            runtime,
            run_id: ticket.run_id().to_string(),
            transferred: false,
        }
    }

    fn mark_launched(&mut self) {
        self.transferred = true;
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.transferred {
            self.runtime.coordination().remove(&self.run_id);
            self.runtime.cancel_unlaunched(&self.run_id);
        }
    }
}

struct ForegroundGuard(Option<pi_core::AbortHandle>);
impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        if let Some(abort) = &self.0 {
            abort.abort();
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
            description: "Delegate to configured subagents.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "enum": self.catalog.profile_names(),
                        "description": "Focused child role"
                    },
                    "context": {
                        "type": "string",
                        "enum": ["fresh", "fork"],
                        "description": "History initialization. Omit to use the role default; fork copies the parent context before this tool batch."
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
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
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
        let parent_session_id = context.session.id()?;
        self.runtime.coordination().bind_session(
            parent_session_id.clone(),
            context.session.handle_for_adapter(),
        );
        let ticket = self
            .runtime
            .begin_launch_with_max_depth(&parent_session_id, profile, self.max_depth)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let mut guard = RunGuard::reserved(self.runtime.clone(), &ticket);
        let run_id = ticket.run_id().to_string();
        let depth = ticket.depth();
        let mut options = launch_plan.into_options();
        if let Some(mode) = input.context {
            options.context = mode;
        }
        let context_mode = options.context;
        let (abort, signal) = pi_core::AbortHandle::new();
        self.runtime.coordination().reserve(&run_id, crate::coordination::ManagedRun {
            owner: parent_session_id.clone(),
            details: json!({"runId":run_id,"agent":profile_name,"depth":depth,"context":context_mode,"state":"running"}),
            abort: abort.clone(), result: None, detached: false,
        });
        let request = IsolatedSessionRequest::new(CustomMessageContent::Text(
            ticket.child_prompt(&input.task),
        ))
        .options(options);
        let handle = match context.session.launch_isolated_session(request).await {
            Ok(handle) => handle,
            Err(error) => return Err(error.into()),
        };
        self.runtime
            .coordination()
            .launched(&run_id, handle.id().as_str());
        updates.send(ToolUpdate {
            content: vec![ContentBlock::Text(TextContent::new(format!(
                "{} subagent running",
                profile_name
            )))],
            details: Some(json!({
                "runId": run_id,
                "isolatedSessionId": handle.id().as_str(),
                "agent": profile_name,
                "depth": depth,
                "context": context_mode,
                "state": "running"
            })),
        });

        let mut changed = self.runtime.coordination().subscribe();
        let (started, readiness) = tokio::sync::oneshot::channel();
        // Cancellation remains armed while the monitor acquires its live wait.
        let mut foreground = ForegroundGuard(Some(abort));
        self.runtime.spawn_monitor(
            parent_session_id.clone(),
            run_id.clone(),
            crate::child_run::ChildRun {
                runtime: self.runtime.downgrade(),
                run_id: run_id.clone(),
                owner: parent_session_id.clone(),
                handle,
                signal,
                timeout,
            }
            .monitor(started),
        );
        guard.mark_launched();
        tokio::select! {
            () = context.signal().wait() => return Err(ToolError::Aborted),
            _ = readiness => {}
        }
        loop {
            let run = self
                .runtime
                .coordination()
                .run(&parent_session_id, &run_id)
                .map_err(ToolError::Execution)?;
            if let Some(result) = run.result {
                foreground.0.take();
                return result.map_err(ToolError::Execution);
            }
            // All foreground waits owned by this session must yield together:
            // a parallel sibling otherwise prevents the parent's next turn.
            if !self
                .runtime
                .coordination()
                .pending(&parent_session_id)
                .is_empty()
            {
                let result = self
                    .runtime
                    .coordination()
                    .detach(&parent_session_id, &run_id)
                    .map_err(ToolError::Execution)?;
                foreground.0.take();
                return Ok(result);
            }
            tokio::select! {
                () = context.signal().wait() => return Err(ToolError::Aborted),
                _ = changed.changed() => {}
            }
        }
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
        context: parsed.context,
    })
}

pub(crate) fn final_text(messages: &[Message]) -> String {
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

pub(crate) fn with_warnings(mut text: String, warnings: &[String]) -> String {
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
        assert_eq!(parsed.context, None);
        let fork = parse_input(
            json!({"agent": "worker", "task": "inspect", "context": "fork"}),
            &catalog,
        )
        .unwrap();
        assert_eq!(fork.context, Some(pi_core::IsolatedContextMode::Fork));
        assert!(
            parse_input(
                json!({"agent": "worker", "task": "inspect", "context": "invalid"}),
                &catalog
            )
            .is_err()
        );
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
        recorded_usage: Mutex<Vec<(Usage, Option<Value>)>>,
        outcome: IsolatedSessionOutcome,
        panic_on_wait: bool,
    }

    #[async_trait]
    impl SessionContextAccess for FakeAccess {
        fn session_id(&self) -> PluginContextResult<String> {
            Ok("root-session".to_string())
        }

        fn active_tools(&self) -> PluginContextResult<Vec<String>> {
            Ok(["read", "grep", "find", "ls", "subagent"]
                .map(str::to_string)
                .to_vec())
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
            assert!(!self.panic_on_wait, "injected child wait panic");
            Ok(self.outcome.clone())
        }

        fn record_usage(&self, usage: Usage, details: Option<Value>) -> PluginContextResult<()> {
            self.recorded_usage.lock().unwrap().push((usage, details));
            Ok(())
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
        recorded_usage: Mutex<Vec<(Usage, Option<Value>)>>,
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
                usage: Usage {
                    input: 11,
                    output: 3,
                    total_tokens: 14,
                    ..Usage::default()
                },
            })
        }

        fn record_usage(&self, usage: Usage, details: Option<Value>) -> PluginContextResult<()> {
            self.recorded_usage.lock().unwrap().push((usage, details));
            Ok(())
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
        let child_usage = Usage {
            input: 120,
            output: 30,
            cache_read: 50,
            total_tokens: 200,
            ..Usage::default()
        };
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
            usage: child_usage.clone(),
        };
        let access = Arc::new(FakeAccess {
            requests: Mutex::new(Vec::new()),
            recorded_usage: Mutex::new(Vec::new()),
            outcome,
            panic_on_wait: false,
        });
        let plugin_access: Arc<dyn PluginContext> = access.clone();
        let epoch = PluginContextEpoch::new(plugin_access);
        let (_abort, signal) = AbortHandle::new();
        let context =
            ToolContext::with_plugin_context(PathBuf::from("/workspace"), signal, epoch.context());
        let (updates, mut update_receiver) = ToolUpdateSink::channel();

        let runtime = SubagentRuntime::default();
        let result = SubagentTool::new(
            runtime.clone(),
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
            result.details.as_ref().unwrap()["usage"],
            json!(child_usage)
        );
        {
            let recorded = access.recorded_usage.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].0, child_usage);
            assert_eq!(recorded[0].1.as_ref().unwrap()["source"], "subagent");
            assert_eq!(
                recorded[0].1.as_ref().unwrap()["childSessionId"],
                "child-session"
            );
        }
        let run_id = result.details.as_ref().unwrap()["runId"].as_str().unwrap();
        assert!(
            runtime
                .coordination()
                .detach("root-session", run_id)
                .is_ok()
        );
        assert!(
            runtime
                .coordination()
                .detach("root-session", run_id)
                .is_ok()
        );
        assert_eq!(access.recorded_usage.lock().unwrap().len(), 1);
        let update_details = update_receiver.recv().await.unwrap().details.unwrap();
        assert_eq!(update_details["state"], "running");
        assert_eq!(update_details["isolatedSessionId"], "isolated-1");
        let requests = access.requests.lock().unwrap();
        let CustomMessageContent::Text(prompt) = &requests[0].input else {
            panic!("expected text child prompt");
        };
        let run_id = run_marker(prompt).expect("child prompt must retain runtime metadata");
        assert_eq!(
            prompt.as_str(),
            format!("<!-- pi-rs-subagent-run:{run_id} -->\nReview the parser")
        );
        assert_eq!(
            requests[0].options.active_tools,
            Some(["read", "grep", "find", "ls"].map(str::to_string).to_vec())
        );
    }

    #[tokio::test]
    async fn configured_zero_depth_blocks_before_creating_a_child_session() {
        let access = Arc::new(FakeAccess {
            requests: Mutex::new(Vec::new()),
            recorded_usage: Mutex::new(Vec::new()),
            outcome: IsolatedSessionOutcome {
                session_id: "unused-child".to_string(),
                messages: Vec::new(),
                aborted: false,
                usage: Usage::default(),
            },
            panic_on_wait: false,
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
            recorded_usage: Mutex::new(Vec::new()),
            outcome: IsolatedSessionOutcome {
                session_id: "child-session".to_string(),
                messages: Vec::new(),
                aborted: false,
                usage: Usage::default(),
            },
            panic_on_wait: false,
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
            recorded_usage: Mutex::new(Vec::new()),
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
        assert_eq!(result.details.as_ref().unwrap()["usage"]["totalTokens"], 14);
        assert_eq!(access.recorded_usage.lock().unwrap().len(), 1);
        assert!(access.aborted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn panicking_child_wait_returns_a_terminal_failure_and_releases_capacity() {
        let access = Arc::new(FakeAccess {
            requests: Mutex::new(Vec::new()),
            recorded_usage: Mutex::new(Vec::new()),
            outcome: IsolatedSessionOutcome {
                session_id: "unused".into(),
                messages: Vec::new(),
                aborted: false,
                usage: Usage::default(),
            },
            panic_on_wait: true,
        });
        let epoch = PluginContextEpoch::new(access);
        let runtime = SubagentRuntime::default();
        let tool = SubagentTool::new(runtime.clone(), SubagentCatalog::builtins(), 1);
        // More sequential failures than the active-run limit must still launch.
        for _ in 0..21 {
            let (_, signal) = AbortHandle::new();
            let context = ToolContext::with_plugin_context(".".into(), signal, epoch.context());
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                tool.execute(
                    context,
                    ToolCallId::new("panic"),
                    json!({"agent":"reviewer","task":"Inspect"}),
                    ToolUpdateSink::channel().0,
                ),
            )
            .await
            .expect("monitor panic must wake the foreground waiter")
            .unwrap_err();
            assert!(error.to_string().contains("panicked"), "{error}");
        }
        assert!(
            runtime
                .coordination()
                .run_ids("root-session", None)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn dropping_an_unpolled_monitor_publishes_failure_and_wakes_waiters() {
        let access = Arc::new(FakeAccess {
            requests: Mutex::new(Vec::new()),
            recorded_usage: Mutex::new(Vec::new()),
            outcome: IsolatedSessionOutcome {
                session_id: "unused".into(),
                messages: vec![],
                aborted: false,
                usage: Usage::default(),
            },
            panic_on_wait: false,
        });
        let epoch = PluginContextEpoch::new(access);
        let runtime = SubagentRuntime::default();
        let ticket = runtime
            .begin_launch("root-session", crate::profiles::builtin_profile("reviewer"))
            .unwrap();
        let (abort, signal) = AbortHandle::new();
        let context = ToolContext::with_plugin_context(".".into(), signal.clone(), epoch.context());
        let handle = context
            .session
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "Inspect".into(),
            )))
            .await
            .unwrap();
        runtime.coordination().reserve(
            ticket.run_id(),
            crate::coordination::ManagedRun {
                owner: "root-session".into(),
                details: json!({}),
                abort,
                result: None,
                detached: false,
            },
        );
        let (started, _) = tokio::sync::oneshot::channel();
        let monitor = crate::child_run::ChildRun {
            runtime: runtime.downgrade(),
            run_id: ticket.run_id().into(),
            owner: "root-session".into(),
            handle,
            signal,
            timeout: None,
        }
        .monitor(started);
        let mut changed = runtime.coordination().subscribe();
        drop(monitor);
        tokio::time::timeout(std::time::Duration::from_secs(1), changed.changed())
            .await
            .unwrap()
            .unwrap();
        let error = runtime
            .coordination()
            .run("root-session", ticket.run_id())
            .unwrap()
            .result
            .unwrap()
            .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(
            runtime
                .coordination()
                .run_ids("root-session", None)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn dropping_runtime_cancels_monitor_without_a_task_owner_cycle() {
        let access = Arc::new(TimeoutAccess {
            requests: Mutex::new(Vec::new()),
            recorded_usage: Mutex::new(Vec::new()),
            aborted: AtomicBool::new(false),
            wake: Notify::new(),
        });
        let epoch = PluginContextEpoch::new(access.clone());
        let runtime = SubagentRuntime::default();
        let weak = runtime.downgrade();
        let ticket = runtime
            .begin_launch("root-session", crate::profiles::builtin_profile("reviewer"))
            .unwrap();
        let (abort, signal) = AbortHandle::new();
        let context = ToolContext::with_plugin_context(".".into(), signal.clone(), epoch.context());
        let handle = context
            .session
            .launch_isolated_session(IsolatedSessionRequest::new(CustomMessageContent::Text(
                "Wait".into(),
            )))
            .await
            .unwrap();
        runtime.coordination().reserve(
            ticket.run_id(),
            crate::coordination::ManagedRun {
                owner: "root-session".into(),
                details: json!({}),
                abort,
                result: None,
                detached: false,
            },
        );
        let (started, ready) = tokio::sync::oneshot::channel();
        runtime.spawn_monitor(
            "root-session".into(),
            ticket.run_id().into(),
            crate::child_run::ChildRun {
                runtime: runtime.downgrade(),
                run_id: ticket.run_id().into(),
                owner: "root-session".into(),
                handle,
                signal,
                timeout: None,
            }
            .monitor(started),
        );
        ready.await.unwrap();
        drop(runtime);
        assert!(
            weak.upgrade().is_none(),
            "monitor must not keep its own runtime alive"
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !access.aborted.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    fn reasoning_model(id: &str) -> ModelSpec {
        let mut model = ModelSpec::new("scripted", id, id, "scripted");
        model.reasoning = true;
        model
    }
}
