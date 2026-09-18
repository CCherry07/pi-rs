use std::sync::{Arc, Mutex};
use std::time::Instant;

use pi_core::{
    AgentPlugin, AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, ContentBlock,
    Message, PluginError, PluginId, PresentationMode, StopReason, Usage,
};
use pi_js_plugin::JsPluginHost;
use pi_sdk::{Config, Pi};
use pi_session::{SessionGenerationOverlay, SubmitOutcome};
use pi_utils::time::unix_timestamp_ms as now_ms;

use crate::model::EVAL_RUN_SCHEMA_VERSION;
use crate::snapshot::{changes, copy_bootstrap_file, copy_fixture, snapshot_workspace};
use crate::{
    ArtifactStore, EvalCase, EvalError, EvalExecutionOutcome, EvalFixture, EvalObservation,
    EvalRun, EvalStep, EvalSystemPrompt, EvalTranscriptEvent, EvalUsage, EvalVariant,
};

pub struct PiEvalHarness {
    artifacts: ArtifactStore,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
}

impl PiEvalHarness {
    pub fn new(artifacts: ArtifactStore) -> Self {
        Self {
            artifacts,
            js_plugin_host: None,
        }
    }

    pub fn with_js_plugin_host(mut self, host: Arc<dyn JsPluginHost>) -> Self {
        self.js_plugin_host = Some(host);
        self
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    pub async fn run(
        &self,
        case: &EvalCase,
        variant: impl Into<EvalVariant>,
        repetition: u32,
        mut config: Config,
    ) -> Result<EvalRun, EvalError> {
        case.validate()?;
        if repetition == 0 {
            return Err(EvalError::InvalidCase(
                "repetition must be greater than zero".to_string(),
            ));
        }
        let variant = variant.into();
        variant.validate()?;
        if (case.requires_js_host || !variant.extensions.is_empty())
            && self.js_plugin_host.is_none()
        {
            return Err(EvalError::Runtime(
                "eval case requires the JavaScript/TypeScript extension host; run it through the Node pi-eval launcher"
                    .to_string(),
            ));
        }

        let provider = config.provider.clone();
        let model = config
            .model
            .clone()
            .unwrap_or_else(|| config.fallback_model.clone());
        let source_agent_dir = config.agent_dir.clone();
        let source_cwd = config.cwd.clone();
        let temporary = tempfile::Builder::new()
            .prefix("pi-eval-")
            .tempdir()
            .map_err(|error| EvalError::Fixture(format!("cannot create eval root: {error}")))?;
        let workspace = temporary.path().join("workspace");
        let isolated_home = temporary.path().join("home");
        let agent_dir = isolated_home.join(".pi/agent");
        let sessions_dir = temporary.path().join("sessions");
        std::fs::create_dir_all(&workspace)
            .and_then(|_| std::fs::create_dir_all(&agent_dir))
            .and_then(|_| std::fs::create_dir_all(&sessions_dir))
            .map_err(|error| {
                EvalError::Fixture(format!("cannot prepare eval directories: {error}"))
            })?;
        if let EvalFixture::Directory(source) = &case.fixture {
            copy_fixture(source, &workspace)?;
        }
        for name in ["auth.json", "models.json"] {
            copy_bootstrap_file(&source_agent_dir, &agent_dir, name)?;
        }
        std::fs::write(
            agent_dir.join("memory.json"),
            "{\"version\":1,\"enabled\":false}\n",
        )
        .map_err(|error| EvalError::Fixture(format!("cannot disable eval memory: {error}")))?;
        let mut settings = serde_json::Map::new();
        settings.insert(
            "shellCommandPrefix".to_string(),
            serde_json::Value::String(isolated_shell_prefix(&isolated_home)),
        );
        if let Some(active_tools) = &case.active_tools {
            settings.insert(
                "defaultTools".to_string(),
                serde_json::to_value(active_tools)
                    .map_err(|error| EvalError::Fixture(error.to_string()))?,
            );
        }
        let encoded = serde_json::to_string_pretty(&settings)
            .map_err(|error| EvalError::Fixture(error.to_string()))?;
        std::fs::write(agent_dir.join("settings.json"), format!("{encoded}\n"))
            .map_err(|error| EvalError::Fixture(format!("cannot write eval settings: {error}")))?;

        let before = snapshot_workspace(&workspace)?;
        let session_path = sessions_dir.join("session.jsonl");
        config.cwd = workspace.clone();
        config.agent_dir = agent_dir.clone();
        config.session_path = session_path.clone();
        config.trust_override = Some(true);
        config.discover_extensions = case.discover_extensions;
        config.load_mcp_config = false;
        config.extensions = config
            .extensions
            .iter()
            .chain(&variant.extensions)
            .map(|source| resolve_extension_source(&source_cwd, source))
            .collect();
        config.native_plugins = config
            .native_plugins
            .iter()
            .chain(&variant.native_plugins)
            .map(|path| resolve_local_path(&source_cwd, path))
            .collect();

        let started_at_ms = now_ms();
        let started = Instant::now();
        let mut builder = Pi::builder(config).presentation_mode(PresentationMode::Print);
        if let Some(js_plugin_host) = &self.js_plugin_host {
            builder = builder.js_plugin_host(Arc::clone(js_plugin_host));
        }
        let host = builder.build().map_err(EvalError::Runtime)?;
        let manager = host.session_manager();
        let prompt_capture = Arc::new(Mutex::new(PromptCapture::default()));
        let mut overlay = SessionGenerationOverlay::new();
        for factory in &case.agent_plugins {
            let factory = Arc::clone(factory);
            overlay = overlay.with_agent_plugin(move || factory());
        }
        overlay = overlay.with_agent_plugin({
            let prompt_capture = Arc::clone(&prompt_capture);
            let treatment = variant.system_prompt;
            move || {
                Arc::new(EvalPromptPlugin {
                    treatment,
                    capture: Arc::clone(&prompt_capture),
                })
            }
        });
        let session = manager
            .create_session_with_overlay(&workspace, &session_path, overlay)
            .await
            .map_err(|error| EvalError::Runtime(error.to_string()))?;

        let mut execution_outcome = EvalExecutionOutcome::Completed;
        let mut errors = Vec::new();
        for step in &case.steps {
            let result = match step {
                EvalStep::Prompt(prompt) => {
                    run_submission(&session, prompt.clone(), case.limits.step_timeout).await
                }
                EvalStep::PromptTemplate(prompt) => {
                    run_submission(
                        &session,
                        render_prompt_template(prompt, &workspace, &agent_dir, &isolated_home),
                        case.limits.step_timeout,
                    )
                    .await
                }
                EvalStep::Reload => {
                    tokio::time::timeout(case.limits.step_timeout, session.reload())
                        .await
                        .map_err(|_| StepError::TimedOut)
                        .and_then(|result| {
                            result.map_err(|error| StepError::Failed(error.to_string()))
                        })
                }
                EvalStep::InvokeCommand { name, arguments } => {
                    let current = session.current();
                    let name = name.clone();
                    let arguments = arguments.clone();
                    run_future_submission(
                        &session,
                        async move { current.invoke_command(&name, &arguments).await },
                        case.limits.step_timeout,
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

        let state = session.current().runtime().agent().state();
        let model_has_pricing = session
            .current()
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
                    errors.push("eval prompt completed without an assistant response".to_string());
                }
                None => {}
            }
        }
        let after = snapshot_workspace(&workspace)?;
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
        let session_jsonl = std::fs::read_to_string(&session_path).ok();
        manager
            .shutdown()
            .await
            .map_err(|error| EvalError::Runtime(format!("session shutdown failed: {error}")))?;

        let mut run = EvalRun {
            schema_version: EVAL_RUN_SCHEMA_VERSION,
            run_id: uuid::Uuid::now_v7().to_string(),
            case_id: case.id.clone(),
            variant: variant.name,
            provider,
            model,
            repetition,
            started_at_ms,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            execution_outcome,
            passed,
            observation,
            grades,
            artifacts: Vec::new(),
        };
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
    treatment: EvalSystemPrompt,
    capture: Arc<Mutex<PromptCapture>>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for EvalPromptPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("pi-eval-system-prompt")
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        let transformed = match self.treatment {
            EvalSystemPrompt::Default => Ok(event.system_prompt),
            EvalSystemPrompt::WithoutPiDocumentation => {
                remove_pi_documentation(&event.system_prompt)
            }
        };
        let mut capture = self
            .capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match transformed {
            Ok(system_prompt) => {
                capture.system_prompt = Some(system_prompt.clone());
                capture.error = None;
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

fn remove_pi_documentation(system_prompt: &str) -> Result<String, String> {
    let documentation_start = system_prompt
        .find("\nPi documentation (read only")
        .ok_or_else(|| "default system prompt has no Pi documentation section".to_string())?;
    let cwd_start = system_prompt[documentation_start..]
        .find("\nCurrent working directory:")
        .map(|offset| documentation_start + offset)
        .ok_or_else(|| {
            "default system prompt has no current-working-directory marker after Pi documentation"
                .to_string()
        })?;
    Ok(format!(
        "{}{}",
        &system_prompt[..documentation_start],
        &system_prompt[cwd_start..]
    ))
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
) -> Result<(), StepError> {
    let current = session.current();
    run_future_submission(
        session,
        async move { current.submit(prompt).await },
        timeout,
    )
    .await
}

async fn run_future_submission<F>(
    session: &pi_session::PiSession,
    future: F,
    step_timeout: std::time::Duration,
) -> Result<(), StepError>
where
    F: std::future::Future<Output = Result<SubmitOutcome, pi_session::SessionError>>
        + Send
        + 'static,
{
    let mut task = tokio::spawn(future);
    match tokio::time::timeout(step_timeout, &mut task).await {
        Ok(Ok(Ok(SubmitOutcome::Agent(_) | SubmitOutcome::Handled))) => Ok(()),
        Ok(Ok(Ok(SubmitOutcome::Queued { .. }))) => Err(StepError::Failed(
            "input was queued while the sequential eval runner was idle".to_string(),
        )),
        Ok(Ok(Ok(_))) => Err(StepError::Failed(
            "submission returned an unsupported outcome".to_string(),
        )),
        Ok(Ok(Err(error))) => Err(StepError::Failed(error.to_string())),
        Ok(Err(error)) => Err(StepError::Failed(format!(
            "eval submission task failed: {error}"
        ))),
        Err(_) => {
            session.current().abort();
            if tokio::time::timeout(std::time::Duration::from_secs(10), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
            Err(StepError::TimedOut)
        }
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

fn resolve_local_path(cwd: &std::path::Path, path: &std::path::Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn resolve_extension_source(cwd: &std::path::Path, source: &str) -> String {
    let path = std::path::Path::new(source);
    if path.is_absolute() {
        return source.to_string();
    }
    let resolved = cwd.join(path);
    if source.starts_with('.') || resolved.exists() {
        resolved.display().to_string()
    } else {
        source.to_string()
    }
}

fn render_prompt_template(
    prompt: &str,
    workspace: &std::path::Path,
    agent_dir: &std::path::Path,
    home: &std::path::Path,
) -> String {
    prompt
        .replace("{{workspace}}", &workspace.display().to_string())
        .replace("{{agent_dir}}", &agent_dir.display().to_string())
        .replace("{{home}}", &home.display().to_string())
}

#[cfg(not(windows))]
fn isolated_shell_prefix(home: &std::path::Path) -> String {
    let quoted = format!("'{}'", home.display().to_string().replace('\'', "'\"'\"'"));
    format!(
        "export HOME={quoted}; unset PI_AGENT_DIR PI_CODING_AGENT_DIR PI_EVAL_ARTIFACT_DIR PI_MODEL PI_PROVIDER PI_REASONING_LEVEL PI_SESSION_FILE PI_SESSION_ID;"
    )
}

#[cfg(windows)]
fn isolated_shell_prefix(home: &std::path::Path) -> String {
    format!(
        "set \"HOME={}\" && set PI_AGENT_DIR= && set PI_CODING_AGENT_DIR= && set PI_EVAL_ARTIFACT_DIR= && set PI_MODEL= && set PI_PROVIDER= && set PI_REASONING_LEVEL= && set PI_SESSION_FILE= && set PI_SESSION_ID= &&",
        home.display()
    )
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::http::header::CONTENT_TYPE;
    use axum::routing::post;

    use super::*;
    use crate::{EvalSystemPrompt, EvalVariant, ExactOutputGrader};

    async fn completion() -> ([(&'static str, &'static str); 1], String) {
        (
            [(CONTENT_TYPE.as_str(), "text/event-stream")],
            concat!(
                "data: {\"id\":\"chatcmpl-eval\",\"object\":\"chat.completion.chunk\",",
                "\"created\":0,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,",
                "\"delta\":{\"role\":\"assistant\",\"content\":\"Paris\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-eval\",\"object\":\"chat.completion.chunk\",",
                "\"created\":0,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,",
                "\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,",
                "\"completion_tokens\":1,\"total_tokens\":9}}\n\n",
                "data: [DONE]\n\n"
            )
            .to_string(),
        )
    }

    #[tokio::test]
    async fn harness_runs_the_product_session_and_persists_native_artifacts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/chat/completions", post(completion)),
            )
            .await
            .unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let source_agent = root.path().join("source-agent");
        std::fs::create_dir_all(&source_agent).unwrap();
        let artifact_directory = root.path().join("artifacts");
        let harness = PiEvalHarness::new(ArtifactStore::new(&artifact_directory).unwrap());
        let case = EvalCase::new("smoke/basic-answer", "product harness smoke")
            .step(EvalStep::Prompt("Capital of France?".to_string()))
            .grader(ExactOutputGrader::new("Paris"))
            .active_tools(Vec::<String>::new());
        let mut config = Config::new(root.path().to_path_buf(), source_agent);
        config.provider = "openai-compatible".to_string();
        config.requested_provider = Some(config.provider.clone());
        config.model = Some("gpt-4o-mini".to_string());
        config.base_url = format!("http://{address}/v1");
        config.api_key = Some("test-key".to_string());
        let run = harness.run(&case, "candidate", 1, config).await.unwrap();
        server.abort();

        assert!(run.passed, "{run:#?}");
        assert_eq!(run.observation.final_response, "Paris");
        assert_eq!(run.observation.usage.total_tokens, 9);
        assert!(
            run.artifacts
                .iter()
                .any(|artifact| artifact.name == "session.jsonl")
        );
        assert!(artifact_directory.join("runs.jsonl").exists());
    }

    #[tokio::test]
    async fn prompt_treatment_runs_as_a_generation_local_native_plugin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/chat/completions", post(completion)),
            )
            .await
            .unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let source_agent = root.path().join("source-agent");
        std::fs::create_dir_all(&source_agent).unwrap();
        let harness =
            PiEvalHarness::new(ArtifactStore::new(root.path().join("artifacts")).unwrap());
        let case = EvalCase::new("prompt/treatment", "prompt treatment")
            .step(EvalStep::Prompt("Capital of France?".to_string()))
            .grader(ExactOutputGrader::new("Paris"))
            .active_tools(Vec::<String>::new());
        let mut config = Config::new(root.path().to_path_buf(), source_agent);
        config.provider = "openai-compatible".to_string();
        config.requested_provider = Some(config.provider.clone());
        config.model = Some("gpt-4o-mini".to_string());
        config.base_url = format!("http://{address}/v1");
        config.api_key = Some("test-key".to_string());
        let run = harness
            .run(
                &case,
                EvalVariant::new("without-docs")
                    .system_prompt(EvalSystemPrompt::WithoutPiDocumentation),
                1,
                config,
            )
            .await
            .unwrap();
        server.abort();

        let prompt = run.observation.system_prompt.unwrap();
        assert!(!prompt.contains("Pi documentation (read only"));
        assert!(prompt.contains("Current working directory:"));
    }

    #[tokio::test]
    async fn explicit_native_plugin_paths_reach_the_product_loader() {
        let root = tempfile::tempdir().unwrap();
        let source_agent = root.path().join("source-agent");
        std::fs::create_dir_all(&source_agent).unwrap();
        let harness =
            PiEvalHarness::new(ArtifactStore::new(root.path().join("artifacts")).unwrap());
        let case = EvalCase::new("native/explicit", "native path")
            .step(EvalStep::Prompt("unused".to_string()));
        let config = Config::new(root.path().to_path_buf(), source_agent);
        let error = harness
            .run(
                &case,
                EvalVariant::new("candidate").native_plugin(root.path().join("missing-plugin")),
                1,
                config,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("missing-plugin"));
    }

    #[tokio::test]
    async fn javascript_cases_fail_before_running_without_the_node_host() {
        let root = tempfile::tempdir().unwrap();
        let source_agent = root.path().join("source-agent");
        let harness =
            PiEvalHarness::new(ArtifactStore::new(root.path().join("artifacts")).unwrap());
        let case = EvalCase::new("javascript/required", "requires JavaScript")
            .step(EvalStep::Prompt("unused".to_string()))
            .requires_js_host(true);
        let error = harness
            .run(
                &case,
                "candidate",
                1,
                Config::new(root.path().to_path_buf(), source_agent),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Node pi-eval launcher"));
    }

    #[test]
    fn documentation_removal_requires_both_stable_markers() {
        let prompt =
            "before\nPi documentation (read only):\n- docs\nCurrent working directory: /tmp";
        assert_eq!(
            remove_pi_documentation(prompt).unwrap(),
            "before\nCurrent working directory: /tmp"
        );
        assert!(remove_pi_documentation("before").is_err());
    }

    #[test]
    fn shell_prefix_isolates_home_and_eval_control_variables() {
        let prefix = isolated_shell_prefix(std::path::Path::new("/tmp/pi eval"));
        assert!(prefix.contains("HOME="));
        assert!(prefix.contains("PI_AGENT_DIR"));
        assert!(prefix.contains("PI_MODEL"));
    }

    #[test]
    fn prompt_templates_expose_only_explicit_isolated_paths() {
        let prompt = render_prompt_template(
            "workspace={{workspace}} agent={{agent_dir}} home={{home}}",
            std::path::Path::new("/tmp/workspace"),
            std::path::Path::new("/tmp/home/.pi/agent"),
            std::path::Path::new("/tmp/home"),
        );
        assert_eq!(
            prompt,
            "workspace=/tmp/workspace agent=/tmp/home/.pi/agent home=/tmp/home"
        );
    }
}
