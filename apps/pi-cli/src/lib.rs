mod clipboard;
mod config;
mod output;
mod plugin_commands;
mod project_trust;
mod runtime_factory;
mod transcript_selection;
mod tui;

use std::io::{IsTerminal, Read};
use std::sync::Arc;

use config::{AppConfig, Cli, CliCommand};
use pi_js_plugin::{JsHostMode, JsPluginHost};
use pi_session::{AgentSessionRuntime, AgentSessionRuntimeTarget, SessionLog};
use project_trust::{ProjectTrustEvaluation, ProjectTrustPromptRequest, ProjectTrustService};
use tokio::sync::mpsc;

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
    if js_host.is_none() && !cli.extensions.is_empty() {
        return Err(
            "--extension requires the Node/NAPI launcher; run `npm start --prefix packages/pi --`"
                .to_string(),
        );
    }
    let mut config = AppConfig::resolve(&cli)?;
    if let Some(CliCommand::Plugin { command }) = &cli.command {
        return plugin_commands::run(&cli, &config, command).await;
    }
    if config.session_path.exists() {
        let (_, document) =
            SessionLog::open(&config.session_path).map_err(|error| error.to_string())?;
        config.cwd = std::fs::canonicalize(&document.header.cwd).map_err(|error| {
            format!(
                "cannot access resumed session cwd {}: {error}",
                document.header.cwd.display()
            )
        })?;
    }
    let input = resolve_input(&cli)?;
    let interactive = !cli.print && !cli.json && std::io::stdin().is_terminal();
    let (project_trust, trust_requests, trusted) =
        resolve_project_trust(&cli, &config, interactive).await?;
    config.trust_project = trusted;
    let target = if config.session_path.exists() {
        AgentSessionRuntimeTarget::open(&config.session_path)
    } else {
        AgentSessionRuntimeTarget::create(&config.cwd, &config.session_path)
    };
    let mut factory =
        runtime_factory::AppSessionFactory::new(config.clone(), project_trust.clone());
    if let Some(js_host) = js_host {
        let mode = if cli.json {
            JsHostMode::Json
        } else if interactive {
            JsHostMode::Tui
        } else {
            JsHostMode::Print
        };
        factory = factory.with_js_plugin_host(js_host, mode);
    }
    let runtime = AgentSessionRuntime::create(factory, target)
        .await
        .map_err(|error| error.to_string())?;
    let session = runtime.session();
    let result = if cli.json {
        let input = input.ok_or_else(|| "--json requires a prompt or stdin".to_string())?;
        output::run_json(Arc::clone(&session), input).await
    } else if cli.print || !interactive {
        let input = input.ok_or_else(|| "print mode requires a prompt or stdin".to_string())?;
        output::run_print(Arc::clone(&session), input).await
    } else {
        tui::run(
            runtime.clone(),
            cli.fullscreen_enabled(),
            input,
            project_trust,
            trust_requests,
        )
        .await
    };
    runtime
        .shutdown()
        .await
        .map_err(|error| error.to_string())?;
    result
}

pub(crate) async fn resolve_project_trust(
    cli: &Cli,
    config: &AppConfig,
    interactive: bool,
) -> Result<
    (
        ProjectTrustService,
        mpsc::UnboundedReceiver<ProjectTrustPromptRequest>,
        bool,
    ),
    String,
> {
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
    Ok((project_trust, trust_requests, trusted))
}

fn resolve_input(cli: &Cli) -> Result<Option<String>, String> {
    if !cli.prompt.is_empty() {
        return Ok(Some(cli.prompt.join(" ")));
    }
    if !std::io::stdin().is_terminal() {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| error.to_string())?;
        let input = input.trim().to_string();
        return Ok((!input.is_empty()).then_some(input));
    }
    Ok(None)
}
