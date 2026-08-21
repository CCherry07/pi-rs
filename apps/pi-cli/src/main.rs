mod clipboard;
mod config;
mod output;
mod project_trust;
mod runtime_factory;
mod transcript_selection;
mod tui;

use std::io::{IsTerminal, Read};
use std::sync::Arc;

use config::{AppConfig, Cli};
use pi_session::{AgentSessionRuntime, AgentSessionRuntimeTarget, SessionLog};
use project_trust::{ProjectTrustEvaluation, ProjectTrustService};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    if let Err(error) = run().await {
        eprintln!("pi: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let cli = Cli::parse_pi();
    let mut config = AppConfig::resolve(&cli)?;
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
    let (project_trust, trust_requests) =
        ProjectTrustService::new(&config.agent_dir, config.trust_override, interactive)
            .map_err(|error| error.to_string())?;
    config.trust_project = match project_trust
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
        .remember(&config.cwd, config.trust_project)
        .map_err(|error| error.to_string())?;
    let target = if config.session_path.exists() {
        AgentSessionRuntimeTarget::open(&config.session_path)
    } else {
        AgentSessionRuntimeTarget::create(&config.cwd, &config.session_path)
    };
    let runtime = AgentSessionRuntime::create(
        runtime_factory::AppSessionFactory::new(config.clone(), project_trust.clone()),
        target,
    )
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
