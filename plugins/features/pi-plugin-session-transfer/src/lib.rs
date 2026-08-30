#![forbid(unsafe_code)]

pub mod export;

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;

use self::export::{
    export_html_file, export_jsonl_file, resolve_user_path, validate_session_import,
};
use async_trait::async_trait;
use pi_core::{
    AgentPlugin, Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel,
    PluginId, RegisterContext, SessionReplacement,
};

const DEFAULT_SHARE_VIEWER_URL: &str = "https://pi.dev/session/";

/// First-party session portability commands.
///
/// The plugin owns command policy and orchestration. Durable session format
/// operations remain in `pi-session`, and confirmation rendering remains in
/// the product frontend behind `UiContext`.
pub struct SessionTransferPlugin {
    uploader: Arc<dyn GistUploader>,
}

impl Default for SessionTransferPlugin {
    fn default() -> Self {
        Self {
            uploader: Arc::new(GitHubCliUploader),
        }
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for SessionTransferPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("session-transfer")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_command(Arc::new(ExportCommand))?;
        context.register_command(Arc::new(ImportCommand))?;
        context.register_command(Arc::new(ShareCommand {
            uploader: Arc::clone(&self.uploader),
        }))?;
        Ok(())
    }
}

struct ExportCommand;

#[async_trait]
impl Command for ExportCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "export".to_string(),
            description: "Export the active session branch as HTML or JSONL".to_string(),
            argument_hint: Some("[file]".to_string()),
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        ensure_not_aborted(&context)?;
        let requested = parse_path_argument(&arguments, "export")?;
        let source = materialized_session_file(&context)?;
        let session_id = context.session.id()?;
        let destination = requested.map_or_else(
            || {
                context
                    .cwd()
                    .join(format!("pi-rs-session-{session_id}.html"))
            },
            |path| resolve_user_path(context.cwd(), &path),
        );
        let export_jsonl = destination
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
        let exported = tokio::task::spawn_blocking(move || {
            if export_jsonl {
                export_jsonl_file(&source, &destination)
            } else {
                export_html_file(&source, &destination)
            }
        })
        .await
        .map_err(|error| CommandError::Execution(format!("session export task failed: {error}")))?
        .map_err(CommandError::Execution)?;
        ensure_not_aborted(&context)?;
        context.ui.notify(
            NoticeLevel::Info,
            format!("Session exported to: {}", exported.display()),
        )?;
        Ok(CommandOutcome::Handled)
    }
}

struct ImportCommand;

#[async_trait]
impl Command for ImportCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "import".to_string(),
            description: "Import and switch to a Pi session file".to_string(),
            argument_hint: Some("<file.jsonl>".to_string()),
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        ensure_not_aborted(&context)?;
        let requested = parse_path_argument(&arguments, "import")?
            .ok_or_else(|| CommandError::InvalidArguments("usage: /import <path.jsonl>".into()))?;
        let source = resolve_user_path(context.cwd(), &requested);
        validate_session_import(&source).map_err(CommandError::Execution)?;

        let confirmed = context
            .ui
            .confirm(
                "Import session?",
                format!(
                    "{}\n\nThe current session in this view will be replaced. The source file is kept.",
                    source.display()
                ),
            )
            .await?;
        if !confirmed {
            context
                .ui
                .notify(NoticeLevel::Info, "Session import cancelled")?;
            return Ok(CommandOutcome::Handled);
        }
        ensure_not_aborted(&context)?;

        let destination = import_destination(&context.session.directory()?, &source);
        let staged_source = source.clone();
        let staged_destination = destination.clone();
        let mut staged = tokio::task::spawn_blocking(move || {
            pi_session::import_session_file(&staged_source, &staged_destination)?;
            Ok::<_, pi_session::SessionError>(StagedImport::new(staged_destination))
        })
        .await
        .map_err(|error| CommandError::Execution(format!("session import task failed: {error}")))?
        .map_err(|error| CommandError::Execution(error.to_string()))?;

        match context.session.switch(&destination).await {
            Ok(SessionReplacement::Replaced(replacement)) => {
                staged.commit();
                replacement.ui.notify(
                    NoticeLevel::Info,
                    format!("Session imported from: {}", source.display()),
                )?;
            }
            Ok(SessionReplacement::Cancelled) => {
                context
                    .ui
                    .notify(NoticeLevel::Info, "Session import cancelled")?;
            }
            Err(error) => {
                return Err(error.into());
            }
        }
        Ok(CommandOutcome::Handled)
    }
}

struct ShareCommand {
    uploader: Arc<dyn GistUploader>,
}

#[async_trait]
impl Command for ShareCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "share".to_string(),
            description: "Share the active session as a secret GitHub gist".to_string(),
            argument_hint: None,
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        if !arguments.trim().is_empty() {
            return Err(CommandError::InvalidArguments("usage: /share".into()));
        }
        ensure_not_aborted(&context)?;
        let source = materialized_session_file(&context)?;
        let uploader = Arc::clone(&self.uploader);
        let shared = tokio::task::spawn_blocking(move || share_session_file(&source, uploader))
            .await
            .map_err(|error| {
                CommandError::Execution(format!("session share task failed: {error}"))
            })?
            .map_err(CommandError::Execution)?;
        ensure_not_aborted(&context)?;
        context.ui.notify(
            NoticeLevel::Info,
            format!(
                "Share URL: {}\nGist: {}",
                shared.viewer_url, shared.gist_url
            ),
        )?;
        Ok(CommandOutcome::Handled)
    }
}

fn materialized_session_file(context: &CommandContext) -> Result<PathBuf, CommandError> {
    context.session.file()?.ok_or_else(|| {
        CommandError::Execution(
            "nothing to export yet; wait for the first assistant response".to_string(),
        )
    })
}

fn ensure_not_aborted(context: &CommandContext) -> Result<(), CommandError> {
    if context.signal().is_aborted() {
        Err(CommandError::Aborted)
    } else {
        Ok(())
    }
}

fn parse_path_argument(arguments: &str, command: &str) -> Result<Option<String>, CommandError> {
    let argument = arguments.trim();
    if argument.is_empty() {
        return Ok(None);
    }
    let Some(quote) = argument
        .chars()
        .next()
        .filter(|character| matches!(character, '\'' | '"'))
    else {
        return Ok(Some(argument.to_string()));
    };
    let quoted = &argument[quote.len_utf8()..];
    let closing = quoted.find(quote).ok_or_else(|| {
        CommandError::InvalidArguments(format!("unterminated quoted path for /{command}"))
    })?;
    if !quoted[closing + quote.len_utf8()..].trim().is_empty() {
        return Err(CommandError::InvalidArguments(format!(
            "unexpected arguments after /{command} path"
        )));
    }
    Ok(Some(quoted[..closing].to_string()))
}

fn import_destination(directory: &Path, source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("imported-session");
    directory.join(format!("{stem}.imported-{}.jsonl", uuid::Uuid::now_v7()))
}

struct StagedImport {
    path: PathBuf,
    committed: bool,
}

impl StagedImport {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for StagedImport {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SharedSession {
    viewer_url: String,
    gist_url: String,
}

trait GistUploader: Send + Sync + 'static {
    fn upload_html(&self, path: &Path) -> Result<SharedSession, String>;
}

struct GitHubCliUploader;

impl GistUploader for GitHubCliUploader {
    fn upload_html(&self, path: &Path) -> Result<SharedSession, String> {
        let authentication = ProcessCommand::new("gh")
            .args(["auth", "status"])
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    "GitHub CLI (gh) is not installed; install it and run `gh auth login`"
                        .to_string()
                } else {
                    format!("failed to check GitHub CLI authentication: {error}")
                }
            })?;
        if !authentication.status.success() {
            return Err("GitHub CLI is not logged in; run `gh auth login` first".to_string());
        }

        let created = ProcessCommand::new("gh")
            .args(["gist", "create", "--public=false"])
            .arg(path)
            .output()
            .map_err(|error| format!("failed to start GitHub CLI: {error}"))?;
        if !created.status.success() {
            let stderr = String::from_utf8_lossy(&created.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("failed to create secret gist: {}", created.status)
            } else {
                format!("failed to create secret gist: {stderr}")
            });
        }
        let stdout = String::from_utf8_lossy(&created.stdout);
        let gist_url = extract_gist_url(&stdout)
            .ok_or_else(|| "GitHub CLI did not return a gist URL".to_string())?;
        let gist_id =
            gist_id(&gist_url).ok_or_else(|| format!("could not parse gist ID from {gist_url}"))?;
        let base = std::env::var("PI_SHARE_VIEWER_URL")
            .unwrap_or_else(|_| DEFAULT_SHARE_VIEWER_URL.to_string());
        Ok(SharedSession {
            viewer_url: viewer_url(&base, gist_id),
            gist_url,
        })
    }
}

fn share_session_file(
    source: &Path,
    uploader: Arc<dyn GistUploader>,
) -> Result<SharedSession, String> {
    let temporary = tempfile::tempdir()
        .map_err(|error| format!("failed to create temporary share directory: {error}"))?;
    let html_path = temporary.path().join("session.html");
    export_html_file(source, &html_path)?;
    uploader.upload_html(&html_path)
}

fn extract_gist_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .rev()
        .find(|part| part.starts_with("https://gist.github.com/"))
        .map(|part| part.trim_end_matches('/').to_string())
}

fn gist_id(url: &str) -> Option<&str> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|part| part.split(['?', '#']).next())
        .filter(|part| !part.is_empty())
}

fn viewer_url(base: &str, gist_id: &str) -> String {
    format!("{}#{gist_id}", base.trim_end_matches('#'))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pi_agent::AgentOptions;
    use pi_core::{Message, PluginContext, PresentationMode, UserMessage};
    use pi_runtime::{PiRuntime, SystemPrompt};
    use pi_session::{
        AgentSession, AgentSessionRuntimeFactory, AgentSessionRuntimeRequest,
        AgentSessionRuntimeTarget, MultiSessionManager, PiPluginContext, PiSession,
        PluginContextBinding, PluginUiBridge, PreparedAgentSession, SessionError, SubmitOutcome,
    };
    use pi_test_support::ScriptedProviderPlugin;

    use super::*;

    struct RecordingUi {
        answer: bool,
        requests: Mutex<Vec<(String, String)>>,
    }

    impl Default for RecordingUi {
        fn default() -> Self {
            Self {
                answer: true,
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl RecordingUi {
        fn rejecting() -> Self {
            Self {
                answer: false,
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl PluginUiBridge for RecordingUi {
        async fn confirm(&self, title: String, message: String) -> Result<bool, String> {
            self.requests.lock().unwrap().push((title, message));
            Ok(self.answer)
        }
    }

    #[derive(Default)]
    struct RecordingUploader {
        html: Mutex<Option<String>>,
    }

    impl GistUploader for RecordingUploader {
        fn upload_html(&self, path: &Path) -> Result<SharedSession, String> {
            *self.html.lock().unwrap() =
                Some(std::fs::read_to_string(path).map_err(|error| error.to_string())?);
            Ok(SharedSession {
                viewer_url: "https://viewer.invalid/#abc".to_string(),
                gist_url: "https://gist.github.com/example/abc".to_string(),
            })
        }
    }

    #[derive(Clone)]
    struct TestFactory {
        binding: PluginContextBinding,
        ui: Arc<RecordingUi>,
        uploader: Arc<RecordingUploader>,
    }

    #[async_trait]
    impl AgentSessionRuntimeFactory for TestFactory {
        fn session_registered(&self, session: &PiSession) {
            self.binding.bind(session.clone());
        }

        async fn prepare(
            &self,
            request: AgentSessionRuntimeRequest,
        ) -> Result<PreparedAgentSession, SessionError> {
            let (cwd, path, create) = match request.target {
                AgentSessionRuntimeTarget::Create { cwd, path, .. } => (cwd, path, true),
                AgentSessionRuntimeTarget::Open { path } => {
                    let (_, document) = pi_session::SessionLog::open(&path)?;
                    (document.header.cwd, path, false)
                }
                AgentSessionRuntimeTarget::Reuse { .. } => {
                    unreachable!("session transfer test does not reuse logs")
                }
            };
            let plugin_context = Arc::new(
                PiPluginContext::new(PresentationMode::Tui, true, self.binding.clone())
                    .with_ui_bridge(self.ui.clone()),
            );
            let context_access: Arc<dyn PluginContext> = plugin_context.clone();
            let runtime = PiRuntime::builder()
                .agent_plugin(SessionTransferPlugin {
                    uploader: self.uploader.clone(),
                })
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .plugin_context(context_access)
                .agent_options(AgentOptions {
                    cwd,
                    ..AgentOptions::default()
                })
                .system_prompt(SystemPrompt::Final("test".to_string()))
                .build()?;
            let prepared = if create {
                AgentSession::prepare_create(runtime, path).await?
            } else {
                AgentSession::prepare_open(runtime, path).await?
            };
            plugin_context.bind_generation_session(prepared.session());
            Ok(prepared)
        }
    }

    #[test]
    fn parses_quoted_paths_without_splitting_spaces() {
        assert_eq!(
            parse_path_argument("\"path with spaces/session.jsonl\"", "import").unwrap(),
            Some("path with spaces/session.jsonl".to_string())
        );
        assert!(
            parse_path_argument("\"unterminated", "import")
                .unwrap_err()
                .to_string()
                .contains("unterminated")
        );
    }

    #[test]
    fn parses_gist_output_and_builds_the_pi_viewer_url() {
        let output = "Created secret gist: https://gist.github.com/example/abc123\n";
        let url = extract_gist_url(output).unwrap();

        assert_eq!(url, "https://gist.github.com/example/abc123");
        assert_eq!(gist_id(&url), Some("abc123"));
        assert_eq!(
            viewer_url("https://pi.dev/session/", "abc123"),
            "https://pi.dev/session/#abc123"
        );
    }

    #[tokio::test]
    async fn registered_commands_export_share_and_import_through_plugin_context() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let session_directory = directory.path().join("sessions");
        let export_directory = directory.path().join("portable files");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&session_directory).unwrap();

        let binding = PluginContextBinding::new();
        let ui = Arc::new(RecordingUi::default());
        let uploader = Arc::new(RecordingUploader::default());
        let sessions = MultiSessionManager::new(TestFactory {
            binding,
            ui: ui.clone(),
            uploader: uploader.clone(),
        });
        let handle = sessions
            .create_session(&workspace, session_directory.join("current.jsonl"))
            .await
            .unwrap();
        let current = handle.current();
        current
            .log()
            .append_message(Message::User(UserMessage::text("portable conversation", 1)))
            .unwrap();
        current.log().materialize().unwrap();
        assert_eq!(
            current
                .runtime()
                .command_specs()
                .into_iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            vec!["export", "import", "share"]
        );

        let jsonl = export_directory.join("portable session.jsonl");
        let html = export_directory.join("portable session.html");
        assert!(matches!(
            current
                .submit(format!("/export \"{}\"", jsonl.display()))
                .await
                .unwrap(),
            SubmitOutcome::Handled
        ));
        assert!(matches!(
            current
                .submit(format!("/export \"{}\"", html.display()))
                .await
                .unwrap(),
            SubmitOutcome::Handled
        ));
        assert!(
            std::fs::read_to_string(&html)
                .unwrap()
                .contains("portable conversation")
        );

        assert!(matches!(
            current.submit("/share").await.unwrap(),
            SubmitOutcome::Handled
        ));
        assert!(
            uploader
                .html
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .contains("portable conversation")
        );

        let previous_path = handle.path();
        let import = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            current.submit(format!("/import \"{}\"", jsonl.display())),
        )
        .await
        .expect("plugin import should not deadlock")
        .unwrap();
        assert!(matches!(import, SubmitOutcome::Handled));
        assert_ne!(handle.path(), previous_path);
        assert!(handle.path().starts_with(&session_directory));
        assert!(jsonl.exists(), "import keeps the portable source file");
        assert_eq!(handle.current().log().load().unwrap().messages().len(), 1);
        {
            let requests = ui.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].0, "Import session?");
            assert!(requests[0].1.contains(jsonl.to_string_lossy().as_ref()));
        }

        sessions.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejected_import_does_not_stage_or_replace_a_session() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let session_directory = directory.path().join("sessions");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&session_directory).unwrap();
        let source = directory.path().join("source.jsonl");
        let source_log = pi_session::SessionLog::create(
            &source,
            pi_session::SessionHeader::new("source", &workspace),
        )
        .unwrap();
        source_log
            .append_message(Message::User(UserMessage::text("keep me", 1)))
            .unwrap();

        let binding = PluginContextBinding::new();
        let sessions = MultiSessionManager::new(TestFactory {
            binding,
            ui: Arc::new(RecordingUi::rejecting()),
            uploader: Arc::new(RecordingUploader::default()),
        });
        let handle = sessions
            .create_session(&workspace, session_directory.join("current.jsonl"))
            .await
            .unwrap();
        let original = handle.path();

        assert!(matches!(
            handle
                .current()
                .submit(format!("/import {}", source.display()))
                .await
                .unwrap(),
            SubmitOutcome::Handled
        ));
        assert_eq!(handle.path(), original);
        assert_eq!(
            std::fs::read_dir(&session_directory)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
                .count(),
            0
        );

        sessions.shutdown().await.unwrap();
    }
}
