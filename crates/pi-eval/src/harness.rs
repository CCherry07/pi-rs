use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pi_core::{ContentBlock, Message, PluginId, StopReason, Usage};
use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, Plugin, PluginError,
};
use pi_session::{MultiSessionManager, SessionGenerationOverlay, SubmitOutcome};
use pi_utils::time::unix_timestamp_ms as now_ms;

use crate::model::EVAL_RUN_SCHEMA_VERSION;
use crate::snapshot::{changes, copy_fixture, snapshot_workspace};
use crate::{
    ArtifactStore, EvalCase, EvalError, EvalExecutionOutcome, EvalFixture, EvalObservation,
    EvalRun, EvalStep, EvalTranscriptEvent, EvalUsage, EvalVariant,
};

/// Isolated paths and initial tool selection supplied to the target preparer.
#[derive(Debug, Clone)]
pub struct EvalRunContext {
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub session_path: PathBuf,
    /// Apply this selection before the target's first dynamic prompt render.
    pub active_tools: Option<Vec<String>>,
}

/// A pure, run-local transformation applied through a reloadable session overlay.
pub type EvalPromptTransform = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync + 'static>;

/// A domain-composed target using the existing managed-session lifecycle.
///
/// Supply a manager dedicated to this run.
/// The runner creates its session and shuts the manager down after execution,
/// including when session creation, observation, or artifact persistence fails.
pub struct PreparedEvalTarget {
    pub manager: MultiSessionManager,
    pub provider: String,
    pub model: String,
    pub template_bindings: BTreeMap<String, String>,
    pub prompt_transform: Option<EvalPromptTransform>,
}

impl PreparedEvalTarget {
    pub fn new(
        manager: MultiSessionManager,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            provider: provider.into(),
            model: model.into(),
            template_bindings: BTreeMap::new(),
            prompt_transform: None,
        }
    }
}

/// Runs evaluations without choosing domain tools, settings, or discovery policy.
pub struct EvalRunner {
    artifacts: ArtifactStore,
    ignored_directories: BTreeSet<String>,
}

impl EvalRunner {
    pub fn new(artifacts: ArtifactStore) -> Self {
        Self {
            artifacts,
            ignored_directories: BTreeSet::new(),
        }
    }

    /// Directory basenames excluded from both fixture copying and observations.
    /// No directories are excluded by default.
    pub fn ignored_directories(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.ignored_directories = names.into_iter().map(Into::into).collect();
        self
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    pub async fn run<F, Fut>(
        &self,
        case: &EvalCase,
        variant: impl Into<EvalVariant>,
        repetition: u32,
        prepare: F,
    ) -> Result<EvalRun, EvalError>
    where
        F: FnOnce(EvalRunContext) -> Fut,
        Fut: Future<Output = Result<PreparedEvalTarget, EvalError>>,
    {
        case.validate()?;
        if repetition == 0 {
            return Err(EvalError::InvalidCase(
                "repetition must be greater than zero".to_string(),
            ));
        }
        let variant = variant.into();
        variant.validate()?;
        let temporary = tempfile::Builder::new()
            .prefix("pi-eval-")
            .tempdir()
            .map_err(|error| EvalError::Fixture(format!("cannot create eval root: {error}")))?;
        let workspace = temporary.path().join("workspace");
        let sessions_dir = temporary.path().join("sessions");
        std::fs::create_dir_all(&workspace)
            .and_then(|_| std::fs::create_dir_all(&sessions_dir))
            .map_err(|error| {
                EvalError::Fixture(format!("cannot prepare eval directories: {error}"))
            })?;
        if let EvalFixture::Directory(source) = &case.fixture {
            copy_fixture(source, &workspace, &self.ignored_directories)?;
        }
        // Preparers may create domain resources in the workspace; retain those changes.
        let before = snapshot_workspace(&workspace, &self.ignored_directories)?;
        let session_path = sessions_dir.join("session.jsonl");
        let started_at_ms = now_ms();
        let started = Instant::now();
        let target = prepare(EvalRunContext {
            root: temporary.path().to_path_buf(),
            workspace: workspace.clone(),
            session_path: session_path.clone(),
            active_tools: case.active_tools.clone(),
        })
        .await?;

        let cleanup = RunCleanup::new(target.manager.clone(), temporary);
        // Keep every fallible operation after preparation inside this scope so
        // the manager is always shut down before returning its result.
        let result = async {
            let prompt_capture = Arc::new(Mutex::new(PromptCapture::default()));
            let mut overlay = SessionGenerationOverlay::new();
            for factory in &case.agent_plugins {
                let factory = Arc::clone(factory);
                overlay = overlay.with_plugin(move || factory());
            }
            overlay = overlay.with_plugin({
                let prompt_capture = Arc::clone(&prompt_capture);
                let transform = target.prompt_transform.clone();
                move || {
                    Arc::new(EvalPromptPlugin {
                        transform: transform.clone(),
                        capture: Arc::clone(&prompt_capture),
                    })
                }
            });
            let session = target
                .manager
                .create_session_with_overlay(&workspace, &session_path, overlay)
                .await
                .map_err(|error| EvalError::Runtime(error.to_string()))?;

            let mut execution_outcome = EvalExecutionOutcome::Completed;
            let mut errors = Vec::new();
            for step in &case.steps {
                let result = match step {
                    EvalStep::Prompt(prompt) => {
                        run_submission(
                            &session,
                            prompt.clone(),
                            case.limits.step_timeout,
                            &cleanup.operation,
                        )
                        .await
                    }
                    EvalStep::PromptTemplate(prompt) => {
                        match render_prompt_template(prompt, &target.template_bindings) {
                            Ok(prompt) => {
                                run_submission(
                                    &session,
                                    prompt,
                                    case.limits.step_timeout,
                                    &cleanup.operation,
                                )
                                .await
                            }
                            Err(error) => Err(StepError::Failed(error)),
                        }
                    }
                    EvalStep::Reload => {
                        let reloading = session.clone();
                        run_operation(
                            &session,
                            async move { reloading.reload().await },
                            case.limits.step_timeout,
                            &cleanup.operation,
                        )
                        .await
                    }
                    EvalStep::InvokeCommand { name, arguments } => {
                        let current = session.current();
                        let name = name.clone();
                        let arguments = arguments.clone();
                        run_future_submission(
                            &session,
                            async move { current.invoke_command(&name, &arguments).await },
                            case.limits.step_timeout,
                            &cleanup.operation,
                        )
                        .await
                    }
                };
                if let Err(error) = result {
                    execution_outcome = match error {
                        StepError::TimedOut => EvalExecutionOutcome::TimedOut,
                        StepError::Failed(_) => EvalExecutionOutcome::Errored,
                    };
                    errors.push(error.to_string());
                    break;
                }
            }

            let captured_prompt = prompt_capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if execution_outcome == EvalExecutionOutcome::Completed
                && let Some(error) = captured_prompt.error
            {
                execution_outcome = EvalExecutionOutcome::Errored;
                errors.push(error);
            }

            let current = session.current();
            let state = current.runtime().agent().state();
            let model_has_pricing = current
                .runtime()
                .model(&state.provider_id, &state.model_id)
                .is_some_and(|model| {
                    model.cost.input > 0.0
                        || model.cost.output > 0.0
                        || model.cost.cache_read > 0.0
                        || model.cost.cache_write > 0.0
                        || !model.cost.tiers.is_empty()
                });
            let final_assistant = state
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::Assistant(message) => Some(message),
                    _ => None,
                });
            if execution_outcome == EvalExecutionOutcome::Completed {
                match final_assistant.map(|message| message.stop_reason) {
                    Some(StopReason::Stop | StopReason::ToolUse) => {}
                    Some(reason) => {
                        execution_outcome = EvalExecutionOutcome::Errored;
                        errors.push(
                            final_assistant
                                .and_then(|message| message.error_message.clone())
                                .unwrap_or_else(|| {
                                    format!("assistant response ended with stop reason {reason:?}")
                                }),
                        );
                    }
                    None if case.steps.iter().any(|step| {
                        matches!(step, EvalStep::Prompt(_) | EvalStep::PromptTemplate(_))
                    }) =>
                    {
                        execution_outcome = EvalExecutionOutcome::Errored;
                        errors.push(
                            "eval prompt completed without an assistant response".to_string(),
                        );
                    }
                    None => {}
                }
            }
            let after = snapshot_workspace(&workspace, &self.ignored_directories)?;
            let mut observation = observe(
                &state.messages,
                captured_prompt.system_prompt,
                model_has_pricing,
                changes(&before, &after),
            );
            observation.errors.extend(errors);
            let grades = case
                .graders
                .iter()
                .map(|grader| grader.grade(&observation))
                .collect::<Vec<_>>();
            let passed = execution_outcome == EvalExecutionOutcome::Completed
                && grades.iter().all(|grade| !grade.required || grade.passed);
            Ok(EvalRun {
                schema_version: EVAL_RUN_SCHEMA_VERSION,
                run_id: uuid::Uuid::now_v7().to_string(),
                case_id: case.id.clone(),
                variant: variant.name,
                provider: target.provider,
                model: target.model,
                repetition,
                started_at_ms,
                duration_ms: 0,
                execution_outcome,
                passed,
                observation,
                grades,
                artifacts: Vec::new(),
            })
        }
        .await;
        let (_temporary, shutdown) = cleanup.finish().await?;
        let mut run = match (result, shutdown) {
            (Ok(run), Ok(())) => run,
            (Err(error), Ok(())) => return Err(error),
            (Ok(_), Err(error)) => {
                return Err(EvalError::Runtime(format!(
                    "session shutdown failed: {error}"
                )));
            }
            (Err(error), Err(shutdown)) => {
                return Err(EvalError::Runtime(format!(
                    "{error}; session shutdown failed: {shutdown}"
                )));
            }
        };
        run.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let session_jsonl = std::fs::read_to_string(&session_path).ok();
        self.artifacts
            .persist_run(&mut run, session_jsonl.as_deref())?;
        Ok(run)
    }
}

#[derive(Debug, Clone, Default)]
struct PromptCapture {
    system_prompt: Option<String>,
    error: Option<String>,
}

struct EvalPromptPlugin {
    transform: Option<EvalPromptTransform>,
    capture: Arc<Mutex<PromptCapture>>,
}

#[pi_plugin::plugin]
impl Plugin for EvalPromptPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("pi-eval-system-prompt")
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        let transformed = match &self.transform {
            Some(transform) => transform(&event.system_prompt),
            None => Ok(event.system_prompt),
        };
        let mut capture = self
            .capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match transformed {
            Ok(system_prompt) => {
                capture.system_prompt = Some(system_prompt.clone());
                Ok(BeforeAgentStartPatch {
                    system_prompt: Some(system_prompt),
                    ..BeforeAgentStartPatch::default()
                })
            }
            Err(error) => {
                capture.error = Some(error);
                Ok(BeforeAgentStartPatch::default())
            }
        }
    }
}

#[derive(Debug)]
enum StepError {
    TimedOut,
    Failed(String),
}

impl std::fmt::Display for StepError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => formatter.write_str("eval step timed out"),
            Self::Failed(message) => write!(formatter, "eval step failed: {message}"),
        }
    }
}

async fn run_submission(
    session: &pi_session::PiSession,
    prompt: String,
    timeout: std::time::Duration,
    operation: &OperationSlot,
) -> Result<(), StepError> {
    let current = session.current();
    run_future_submission(
        session,
        async move { current.submit(prompt).await },
        timeout,
        operation,
    )
    .await
}

async fn run_future_submission<F>(
    session: &pi_session::PiSession,
    future: F,
    step_timeout: std::time::Duration,
    operation: &OperationSlot,
) -> Result<(), StepError>
where
    F: Future<Output = Result<SubmitOutcome, pi_session::SessionError>> + Send + 'static,
{
    match run_operation(session, future, step_timeout, operation).await? {
        SubmitOutcome::Agent(_) | SubmitOutcome::Handled => Ok(()),
        SubmitOutcome::Queued { .. } => Err(StepError::Failed(
            "input was queued while the sequential eval runner was idle".to_string(),
        )),
        _ => Err(StepError::Failed(
            "submission returned an unsupported outcome".to_string(),
        )),
    }
}

// The cleanup task owns the workspace and the active operation independently of
// the caller. Dropping the caller only closes the signal; cleanup itself remains
// owned until the same shutdown future has completed.
type OperationSlot = Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>;
type CleanupResult = (
    tempfile::TempDir,
    Result<(), pi_session::MultiSessionManagerError>,
);

struct RunCleanup {
    operation: OperationSlot,
    finish: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<CleanupResult>,
}

impl RunCleanup {
    fn new(manager: MultiSessionManager, temporary: tempfile::TempDir) -> Self {
        let operation = OperationSlot::default();
        let pending = Arc::clone(&operation);
        let (finish, finished) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = finished.await;
            for session in manager.sessions() {
                session.abort();
            }
            let active = pending.lock().await.take();
            if let Some(active) = active {
                active.abort();
                let _ = active.await;
            }
            // Manager lifecycle transactions complete even when their requesting
            // future is dropped. Shutdown waits for that existing operation gate.
            let result = manager.shutdown().await;
            (temporary, result)
        });
        Self {
            operation,
            finish,
            task,
        }
    }

    async fn finish(self) -> Result<CleanupResult, EvalError> {
        let _ = self.finish.send(());
        self.task
            .await
            .map_err(|error| EvalError::Runtime(format!("eval cleanup task failed: {error}")))
    }
}

async fn run_operation<F, T, E>(
    session: &pi_session::PiSession,
    future: F,
    step_timeout: std::time::Duration,
    operation: &OperationSlot,
) -> Result<T, StepError>
where
    F: Future<Output = Result<T, E>> + Send + 'static,
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    {
        let mut active = operation.lock().await;
        *active = Some(tokio::spawn(async move {
            let result = future.await.map_err(|error| error.to_string());
            let _ = sender.send(result);
        }));
    }
    let result = tokio::time::timeout(step_timeout, &mut receiver).await;
    let timed_out = result.is_err();
    if timed_out {
        session.abort();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), &mut receiver).await;
    }
    // Await the JoinHandle in place. If the caller is cancelled during this
    // await, the mutex guard drops but the cleanup task still owns the handle.
    let joined = {
        let mut active = operation.lock().await;
        let task = active.as_mut().expect("one sequential eval operation");
        if timed_out {
            task.abort();
        }
        let joined = task.await;
        active.take();
        joined
    };
    if timed_out {
        return Err(StepError::TimedOut);
    }
    if let Err(error) = joined {
        return Err(StepError::Failed(format!("eval step task failed: {error}")));
    }
    match result {
        Ok(Ok(Ok(outcome))) => Ok(outcome),
        Ok(Ok(Err(error))) => Err(StepError::Failed(error)),
        Ok(Err(error)) => Err(StepError::Failed(format!("eval step task failed: {error}"))),
        Err(_) => unreachable!("timeout handled above"),
    }
}

fn observe(
    messages: &[Message],
    system_prompt: Option<String>,
    model_has_pricing: bool,
    workspace_changes: Vec<crate::WorkspaceChange>,
) -> EvalObservation {
    let mut transcript = Vec::new();
    let mut usage = EvalUsage::default();
    let mut final_response = String::new();
    for message in messages {
        match message {
            Message::User(message) => transcript.push(EvalTranscriptEvent::Message {
                role: "user".to_string(),
                content: content_text(&message.content),
            }),
            Message::Assistant(message) => {
                let text = content_text(&message.content);
                final_response.clone_from(&text);
                if !text.is_empty() {
                    transcript.push(EvalTranscriptEvent::Message {
                        role: "assistant".to_string(),
                        content: text,
                    });
                }
                add_usage(&mut usage, &message.usage, model_has_pricing);
                for call in message.tool_calls() {
                    usage.tool_calls = usage.tool_calls.saturating_add(1);
                    transcript.push(EvalTranscriptEvent::ToolCall {
                        id: call.id.to_string(),
                        name: call.name,
                        arguments: call.arguments,
                    });
                }
            }
            Message::ToolResult(message) => transcript.push(EvalTranscriptEvent::ToolResult {
                tool_call_id: message.tool_call_id.to_string(),
                name: message.tool_name.clone(),
                content: content_text(&message.content),
                details: message.details.clone(),
                is_error: message.is_error,
            }),
            Message::Custom(message) => transcript.push(EvalTranscriptEvent::Custom {
                custom_type: message.custom_type.clone(),
                content: content_text(&message.content.to_blocks()),
            }),
        }
    }
    EvalObservation {
        system_prompt,
        final_response,
        transcript,
        workspace_changes,
        usage,
        errors: Vec::new(),
    }
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn add_usage(summary: &mut EvalUsage, usage: &Usage, model_has_pricing: bool) {
    summary.input_tokens = summary.input_tokens.saturating_add(usage.input);
    summary.output_tokens = summary.output_tokens.saturating_add(usage.output);
    summary.cache_read_tokens = summary.cache_read_tokens.saturating_add(usage.cache_read);
    summary.cache_write_tokens = summary.cache_write_tokens.saturating_add(usage.cache_write);
    summary.total_tokens = summary.total_tokens.saturating_add(usage.total_tokens);
    if model_has_pricing {
        summary.estimated_cost_usd =
            Some(summary.estimated_cost_usd.unwrap_or_default() + usage.cost.total);
    }
}

fn render_prompt_template(
    prompt: &str,
    bindings: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut rendered = String::new();
    let mut remaining = prompt;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let token = &remaining[start + 2..];
        let end = token
            .find("}}")
            .ok_or_else(|| "unterminated prompt template token".to_string())?;
        let name = &token[..end];
        let value = bindings
            .get(name)
            .ok_or_else(|| format!("unbound prompt template token: {{{{{name}}}}}"))?;
        rendered.push_str(value);
        remaining = &token[end + 2..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use pi_sdk::{AgentHost, ModelSelection};

    use super::*;

    #[test]
    fn prompt_templates_use_explicit_bindings_without_recursive_expansion() {
        let bindings = BTreeMap::from([
            ("order".into(), "ORD-42".into()),
            ("literal".into(), "{{not_a_binding}}".into()),
        ]);
        assert_eq!(
            render_prompt_template("{{order}} / {{literal}} / {{order}}", &bindings).unwrap(),
            "ORD-42 / {{not_a_binding}} / ORD-42"
        );
        assert!(
            render_prompt_template("{{home}}", &bindings)
                .unwrap_err()
                .contains("unbound")
        );
        assert!(
            render_prompt_template("{{order", &bindings)
                .unwrap_err()
                .contains("unterminated")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn noncooperative_step_is_joined_after_forced_cancellation() {
        struct Cleanup(Arc<AtomicBool>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let host = AgentHost::builder(ModelSelection::new("scripted", "test"), "test")
            .provider_plugin_factory(|| pi_test_support::ScriptedProviderPlugin::scripted([]))
            .build();
        let session = host
            .sessions()
            .create_session(directory.path(), directory.path().join("session.jsonl"))
            .await
            .unwrap();
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup = Cleanup(Arc::clone(&cleaned));
        let result = run_operation(
            &session,
            async move {
                let _cleanup = cleanup;
                std::future::pending::<Result<(), String>>().await
            },
            std::time::Duration::from_millis(1),
            &OperationSlot::default(),
        )
        .await;
        assert!(matches!(result, Err(StepError::TimedOut)));
        assert!(
            cleaned.load(Ordering::SeqCst),
            "task must be dropped before returning"
        );
        host.sessions().shutdown().await.unwrap();
    }
}
