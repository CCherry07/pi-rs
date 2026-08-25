use std::io::IsTerminal;

use pi_js_package_manager::{
    ManageOperation, ManageResult, PackageManager, PackageScope, ResolveRequest,
};

use crate::config::{AppConfig, Cli, CliCommand};

pub(crate) async fn run(cli: &Cli, config: &AppConfig, command: &CliCommand) -> Result<(), String> {
    let interactive = !matches!(command, CliCommand::Update { .. })
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal();
    let trust = crate::resolve_project_trust(cli, config, interactive).await?;
    let mut manager = PackageManager::new(ResolveRequest {
        cwd: config.cwd.clone(),
        agent_dir: config.agent_dir.clone(),
        project_trusted: trust.trusted(),
        explicit_sources: Vec::new(),
        discover_extensions: true,
    });

    let operation = operation(command)?;
    let requested_update_source = match &operation {
        ManageOperation::Update { source } => source.clone(),
        ManageOperation::Install { .. }
        | ManageOperation::Remove { .. }
        | ManageOperation::List => None,
    };
    let result = manager
        .manage(operation)
        .await
        .map_err(|error| error.to_string())?;
    render_result(result, requested_update_source.as_deref())
}

fn operation(command: &CliCommand) -> Result<ManageOperation, String> {
    match command {
        CliCommand::Install { source, local } => Ok(ManageOperation::Install {
            source: source.clone(),
            scope: scope(*local),
        }),
        CliCommand::Remove { source, local } => Ok(ManageOperation::Remove {
            source: source.clone(),
            scope: scope(*local),
        }),
        CliCommand::List => Ok(ManageOperation::List),
        CliCommand::Update {
            source,
            extensions,
            extension,
            self_update,
            models,
            all,
            force: _,
        } => {
            if *models {
                return Err(
                    "model catalog refresh is not part of JavaScript package management"
                        .to_string(),
                );
            }
            if *self_update || *all {
                return Err(
                    "pi-rs self-update is not implemented; use `pi update --extensions` to update JavaScript packages"
                        .to_string(),
                );
            }
            if source.is_some() && extension.is_some() {
                return Err(
                    "positional update targets cannot be combined with --extension".to_string(),
                );
            }
            if *extensions && (source.is_some() || extension.is_some()) {
                return Err(
                    "--extensions cannot be combined with a specific extension source".to_string(),
                );
            }
            let requested = extension.clone().or_else(|| source.clone());
            if requested.as_deref().is_some_and(|source| {
                source == "self" || source == "pi" || source == "pi-coding-agent"
            }) {
                return Err(
                    "pi-rs self-update is not implemented; use `pi update --extensions` to update JavaScript packages"
                        .to_string(),
                );
            }
            if requested.is_none() && !extensions {
                return Err(
                    "bare `pi update` targets the Pi executable; self-update is not implemented in pi-rs. Use `pi update --extensions`."
                        .to_string(),
                );
            }
            Ok(ManageOperation::Update { source: requested })
        }
        CliCommand::Auth { .. } | CliCommand::Plugin { .. } => {
            Err("not a JavaScript package command".to_string())
        }
    }
}

fn render_result(
    result: ManageResult,
    requested_update_source: Option<&str>,
) -> Result<(), String> {
    match result {
        ManageResult::Installed { source, .. } => println!("Installed {source}"),
        ManageResult::Removed {
            source,
            configured: true,
            ..
        } => println!("Removed {source}"),
        ManageResult::Removed {
            source,
            configured: false,
            ..
        } => return Err(format!("No matching package found for {source}")),
        ManageResult::Updated { .. } => {
            println!("{}", update_confirmation(requested_update_source));
        }
        ManageResult::Listed { packages } => {
            if packages.is_empty() {
                println!("No packages installed.");
                return Ok(());
            }
            let user: Vec<_> = packages
                .iter()
                .filter(|package| package.scope == PackageScope::User)
                .collect();
            let project: Vec<_> = packages
                .iter()
                .filter(|package| package.scope == PackageScope::Project)
                .collect();
            if !user.is_empty() {
                println!("User packages:");
                render_packages(&user);
            }
            if !project.is_empty() {
                if !user.is_empty() {
                    println!();
                }
                println!("Project packages:");
                render_packages(&project);
            }
        }
    }
    Ok(())
}

fn update_confirmation(requested_source: Option<&str>) -> String {
    requested_source.map_or_else(
        || "Updated packages".to_string(),
        |source| format!("Updated {source}"),
    )
}

fn render_packages(packages: &[&pi_js_package_manager::ConfiguredPackage]) {
    for package in packages {
        let filtered = if package.filtered { " (filtered)" } else { "" };
        println!("  {}{filtered}", package.source);
        if let Some(path) = &package.installed_path {
            println!("    {}", path.display());
        }
    }
}

fn scope(local: bool) -> PackageScope {
    if local {
        PackageScope::Project
    } else {
        PackageScope::User
    }
}

#[cfg(test)]
mod tests {
    use super::{operation, update_confirmation};
    use crate::config::{Cli, CliCommand};

    fn command(arguments: &[&str]) -> CliCommand {
        Cli::try_parse_pi_from(
            arguments
                .iter()
                .map(|argument| (*argument).to_string())
                .collect(),
        )
        .unwrap()
        .command
        .unwrap()
    }

    #[test]
    fn update_requires_an_extension_target_until_self_update_exists() {
        assert!(operation(&command(&["update"])).is_err());
        assert!(operation(&command(&["update", "--extensions"])).is_ok());
        assert!(operation(&command(&["update", "npm:example"])).is_ok());
        assert!(operation(&command(&["update", "--extensions", "--force"])).is_ok());
        assert!(operation(&command(&["update", "--self"])).is_err());
    }

    #[test]
    fn targeted_update_echoes_the_requested_source() {
        assert_eq!(update_confirmation(Some(".")), "Updated .");
        assert_eq!(update_confirmation(None), "Updated packages");
    }
}
