#![warn(unreachable_pub)]

mod auth;
mod clipboard;
mod config;
mod output;
mod package_commands;
mod plugin_commands;
mod project_trust;
mod session_factory;
mod text_selection;
mod tui;

use std::io::{IsTerminal, Read};
use std::sync::Arc;

use config::{AppConfig, Cli, CliCommand};
use pi_js_package_manager::PackageManager as JsPackageManager;
use pi_js_plugin::{ExtensionSessionBinding, JsHostMode, JsPluginHost};
use pi_session::{MultiSessionManager, PiSession, SessionLog};
use project_trust::{ProjectTrustEvaluation, ProjectTrustPromptRequest, ProjectTrustService};
use tokio::sync::mpsc;

pub(crate) struct ResolvedProjectTrust {
    service: ProjectTrustService,
    requests: mpsc::UnboundedReceiver<ProjectTrustPromptRequest>,
    trusted: bool,
}

impl ResolvedProjectTrust {
    pub(crate) fn trusted(&self) -> bool {
        self.trusted
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CLIMode {
    Tui { initial_prompt: Option<String> },
    Print { input: String },
    Json { input: String },
}

impl CLIMode {
    fn resolve(cli: &Cli, input: Option<String>, stdin_is_terminal: bool) -> Result<Self, String> {
        if cli.json {
            return input
                .map(|input| Self::Json { input })
                .ok_or_else(|| "--json requires a prompt or stdin".to_string());
        }
        if cli.print || !stdin_is_terminal {
            return input
                .map(|input| Self::Print { input })
                .ok_or_else(|| "--print requires a prompt or stdin".to_string());
        }
        Ok(Self::Tui {
            initial_prompt: input,
        })
    }

    fn is_interactive(&self) -> bool {
        matches!(self, Self::Tui { .. })
    }

    fn js_host_mode(&self) -> JsHostMode {
        match self {
            Self::Tui { .. } => JsHostMode::Tui,
            Self::Print { .. } => JsHostMode::Print,
            Self::Json { .. } => JsHostMode::Json,
        }
    }
}

/// Runs the product using the process argument vector and only native Rust plugins.
pub async fn run_from_env() -> Result<(), String> {
    run(Cli::parse_pi(), None).await
}

/// Runs the product as a Node-hosted application. `arguments` excludes the
/// executable name, matching `process.argv.slice(2)`.
pub async fn run_with_js_host(
    arguments: Vec<String>,
    js_host: Arc<dyn JsPluginHost>,
) -> Result<(), String> {
    let cli = match Cli::try_parse_pi_from(arguments) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return Ok(());
        }
        Err(error) => return Err(error.to_string()),
    };
    run(cli, Some(js_host)).await
}

async fn run(cli: Cli, js_host: Option<Arc<dyn JsPluginHost>>) -> Result<(), String> {
    let mut config = AppConfig::resolve(&cli)?;
    if !matches!(cli.command, Some(CliCommand::Auth { .. })) {
        auth::refresh_oauth_if_needed(&config.agent_dir).await?;
    }
    if let Some(CliCommand::Auth { command }) = &cli.command {
        return auth::run(&config.agent_dir, command).await;
    }
    if let Some(CliCommand::Plugin { command }) = &cli.command {
        return plugin_commands::run(&cli, &config, command).await;
    }
    if let Some(
        command @ (CliCommand::Install { .. }
        | CliCommand::Remove { .. }
        | CliCommand::List
        | CliCommand::Update { .. }),
    ) = &cli.command
    {
        return package_commands::run(&cli, &config, command).await;
    }
    let session_exists = config.session_path.exists();
    if session_exists {
        let (_, document) =
            SessionLog::open(&config.session_path).map_err(|error| error.to_string())?;
        config.cwd = std::fs::canonicalize(&document.header.cwd).map_err(|error| {
            format!(
                "cannot access resumed session cwd {}: {error}",
                document.header.cwd.display()
            )
        })?;
    }
    let stdin_is_terminal = std::io::stdin().is_terminal();
    let input = resolve_input(&cli, stdin_is_terminal)?;
    let cli_mode = CLIMode::resolve(&cli, input, stdin_is_terminal)?;
    let trust = resolve_project_trust(&cli, &config, cli_mode.is_interactive()).await?;
    ensure_javascript_host_available(&config, trust.trusted(), js_host.is_some())?;
    let cwd = config.cwd.clone();
    let agent_dir = config.agent_dir.clone();
    let session_path = config.session_path.clone();
    let mut factory = session_factory::ProductSessionFactory::new(config, trust.service.clone());
    let js_session_binding = js_host.as_ref().map(|_| ExtensionSessionBinding::new());
    if let (Some(js_host), Some(session_binding)) = (js_host, js_session_binding.clone()) {
        factory = factory.with_js_plugin_host(js_host, cli_mode.js_host_mode(), session_binding);
    }
    let sessions = MultiSessionManager::new(factory);
    let session = if session_exists {
        sessions.open_session(&session_path).await
    } else {
        sessions.create_session(&cwd, &session_path).await
    }
    .map_err(|error| error.to_string())?;
    if let Some(binding) = &js_session_binding {
        binding.bind(session.clone());
    }
    let result = run_cli_with_shutdown(
        cli_mode,
        session,
        cli.fullscreen_enabled(),
        trust.service,
        trust.requests,
        agent_dir,
        js_session_binding,
    )
    .await;
    let shutdown = sessions.shutdown().await.map_err(|error| error.to_string());
    finish_run(result, shutdown)
}

fn ensure_javascript_host_available(
    config: &AppConfig,
    project_trusted: bool,
    host_available: bool,
) -> Result<(), String> {
    if host_available
        || !JsPackageManager::new(config.javascript_resolve_request(project_trusted))
            .requires_javascript_host()
    {
        return Ok(());
    }
    Err(
        "JavaScript/TypeScript extensions are active for this run, but this executable uses the native-only Rust adapter.\n\
From a pi-rs source checkout, run `./scripts/pi-dev ...`; installed users should run the `@pi-rs/cli` npm launcher.\n\
Pass `--no-extensions` to disable configured and automatic extensions; explicit `-e`/`--extension` sources must also be removed."
            .to_string(),
    )
}

async fn run_cli_with_shutdown(
    mode: CLIMode,
    session: PiSession,
    fullscreen: bool,
    project_trust: ProjectTrustService,
    trust_requests: mpsc::UnboundedReceiver<ProjectTrustPromptRequest>,
    agent_dir: std::path::PathBuf,
    shutdown: Option<ExtensionSessionBinding>,
) -> Result<(), String> {
    if let Some(shutdown) = shutdown {
        let abort_session = session.clone();
        tokio::select! {
            result = run_cli(
                mode,
                session,
                fullscreen,
                project_trust,
                trust_requests,
                agent_dir,
            ) => result,
            () = shutdown.wait_for_shutdown() => {
                abort_session.abort();
                Ok(())
            }
        }
    } else {
        run_cli(
            mode,
            session,
            fullscreen,
            project_trust,
            trust_requests,
            agent_dir,
        )
        .await
    }
}

async fn run_cli(
    mode: CLIMode,
    session: PiSession,
    fullscreen: bool,
    project_trust: ProjectTrustService,
    trust_requests: mpsc::UnboundedReceiver<ProjectTrustPromptRequest>,
    agent_dir: std::path::PathBuf,
) -> Result<(), String> {
    match mode {
        CLIMode::Json { input } => output::run_json(session, input).await,
        CLIMode::Print { input } => output::run_print(session, input).await,
        CLIMode::Tui { initial_prompt } => {
            tui::run(
                session,
                fullscreen,
                initial_prompt,
                project_trust,
                trust_requests,
                agent_dir,
            )
            .await
        }
    }
}

fn finish_run(result: Result<(), String>, shutdown: Result<(), String>) -> Result<(), String> {
    match (result, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown_error)) => Err(format!(
            "{error}\nmulti-session shutdown also failed: {shutdown_error}"
        )),
    }
}

pub(crate) async fn resolve_project_trust(
    cli: &Cli,
    config: &AppConfig,
    interactive: bool,
) -> Result<ResolvedProjectTrust, String> {
    let (project_trust, trust_requests) =
        ProjectTrustService::new(&config.agent_dir, config.trust_override, interactive)
            .map_err(|error| error.to_string())?;
    let trusted = match project_trust
        .evaluate(&config.cwd)
        .map_err(|error| error.to_string())?
    {
        ProjectTrustEvaluation::Known(trusted) => trusted,
        ProjectTrustEvaluation::Ask(options) => {
            let selected =
                tui::select_project_trust(cli.fullscreen_enabled(), &config.cwd, &options).await?;
            selected
                .and_then(|index| options.get(index))
                .map(|option| project_trust.apply_option(&config.cwd, option))
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or(false)
        }
    };
    project_trust
        .remember(&config.cwd, trusted)
        .map_err(|error| error.to_string())?;
    Ok(ResolvedProjectTrust {
        service: project_trust,
        requests: trust_requests,
        trusted,
    })
}

fn resolve_input(cli: &Cli, stdin_is_terminal: bool) -> Result<Option<String>, String> {
    if !cli.prompt.is_empty() {
        return Ok(Some(cli.prompt.join(" ")));
    }
    if !stdin_is_terminal {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| error.to_string())?;
        let input = input.trim().to_string();
        return Ok((!input.is_empty()).then_some(input));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::{CLIMode, Cli, JsHostMode, ensure_javascript_host_available, finish_run};
    use crate::config::AppConfig;

    fn cli(arguments: &[&str]) -> Cli {
        Cli::try_parse_pi_from(
            arguments
                .iter()
                .map(|argument| (*argument).to_string())
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn resolves_each_frontend_through_one_mode_boundary() {
        let tui = CLIMode::resolve(&cli(&[]), None, true).unwrap();
        assert_eq!(
            tui,
            CLIMode::Tui {
                initial_prompt: None
            }
        );
        assert!(tui.is_interactive());
        assert_eq!(tui.js_host_mode(), JsHostMode::Tui);

        let print = CLIMode::resolve(&cli(&[]), Some("piped".to_string()), false).unwrap();
        assert_eq!(
            print,
            CLIMode::Print {
                input: "piped".to_string()
            }
        );
        assert!(!print.is_interactive());
        assert_eq!(print.js_host_mode(), JsHostMode::Print);

        let json = CLIMode::resolve(
            &cli(&["--print", "--json"]),
            Some("prompt".to_string()),
            true,
        )
        .unwrap();
        assert_eq!(json.js_host_mode(), JsHostMode::Json);
    }

    #[test]
    fn noninteractive_frontends_require_input_before_application_startup() {
        assert_eq!(
            CLIMode::resolve(&cli(&["--json"]), None, true),
            Err("--json requires a prompt or stdin".to_string())
        );
        assert_eq!(
            CLIMode::resolve(&cli(&["--print"]), None, true),
            Err("--print requires a prompt or stdin".to_string())
        );
        assert_eq!(
            CLIMode::resolve(&cli(&[]), None, false),
            Err("--print requires a prompt or stdin".to_string())
        );
    }

    #[test]
    fn native_adapter_rejects_active_javascript_configuration() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let agent_dir = root.path().join("agent");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(
            agent_dir.join("settings.json"),
            serde_json::to_vec(&json!({"packages": ["npm:todo-extension"]})).unwrap(),
        )
        .unwrap();
        let config = AppConfig::resolve(&cli(&[
            "--cwd",
            cwd.to_str().unwrap(),
            "--agent-dir",
            agent_dir.to_str().unwrap(),
        ]))
        .unwrap();

        let error = ensure_javascript_host_available(&config, true, false).unwrap_err();
        assert!(error.contains("native-only Rust adapter"));
        assert!(error.contains("./scripts/pi-dev"));
        assert!(error.contains("--no-extensions"));
        assert!(ensure_javascript_host_available(&config, true, true).is_ok());

        let disabled = AppConfig::resolve(&cli(&[
            "--cwd",
            cwd.to_str().unwrap(),
            "--agent-dir",
            agent_dir.to_str().unwrap(),
            "--no-extensions",
        ]))
        .unwrap();
        assert!(ensure_javascript_host_available(&disabled, true, false).is_ok());
    }

    #[test]
    fn frontend_error_stays_primary_when_shutdown_also_fails() {
        assert_eq!(
            finish_run(
                Err("frontend failed".to_string()),
                Err("shutdown failed".to_string())
            ),
            Err("frontend failed\nmulti-session shutdown also failed: shutdown failed".to_string())
        );
        assert_eq!(
            finish_run(Ok(()), Err("shutdown failed".to_string())),
            Err("shutdown failed".to_string())
        );
    }
}
