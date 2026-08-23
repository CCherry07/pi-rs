use std::io::IsTerminal;

use pi_plugin_manager::{InstallScope, PluginManager, PluginManagerOptions};

use crate::config::{AppConfig, Cli, PluginCommand};

pub(crate) async fn run(
    cli: &Cli,
    config: &AppConfig,
    command: &PluginCommand,
) -> Result<(), String> {
    let local = match command {
        PluginCommand::Install { local, .. }
        | PluginCommand::List { local }
        | PluginCommand::Sync { local, .. }
        | PluginCommand::Remove { local, .. } => *local,
    };
    if local {
        let trust = crate::resolve_project_trust(
            cli,
            config,
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        )
        .await?;
        if !trust.trusted() {
            return Err(
                "project-local plugin management requires a trusted project; pass --approve to trust it for this command"
                    .to_string(),
            );
        }
    }
    let scope = if local {
        InstallScope::Project
    } else {
        InstallScope::Global
    };
    let mut options = PluginManagerOptions::new(&config.cwd, &config.agent_dir);
    match command {
        PluginCommand::Install { registry, .. } | PluginCommand::Sync { registry, .. } => {
            options.registry = registry.clone();
        }
        PluginCommand::List { .. } | PluginCommand::Remove { .. } => {}
    }
    let manager = PluginManager::new(options).map_err(|error| error.to_string())?;
    match command {
        PluginCommand::Install {
            source, version, ..
        } => {
            let installed = manager
                .install(source, version.as_deref(), scope)
                .await
                .map_err(|error| error.to_string())?;
            println!(
                "Installed {} {} ({}, {})",
                installed.id, installed.version, installed.kind, installed.target
            );
        }
        PluginCommand::List { .. } => {
            let installed = manager.list(scope).map_err(|error| error.to_string())?;
            if installed.is_empty() {
                println!("No {} native plugins installed.", scope_name(scope));
            } else {
                for plugin in installed {
                    println!(
                        "{}\t{}\t{}\t{}",
                        plugin.id, plugin.version, plugin.kind, plugin.source
                    );
                }
            }
        }
        PluginCommand::Sync { .. } => {
            let installed = manager
                .sync(scope)
                .await
                .map_err(|error| error.to_string())?;
            println!(
                "Synchronized {} {} native plugin(s).",
                installed.len(),
                scope_name(scope)
            );
        }
        PluginCommand::Remove { id, .. } => {
            let removed = manager
                .remove(id, scope)
                .map_err(|error| error.to_string())?;
            println!("Removed {} {}", removed.id, removed.version);
        }
    }
    Ok(())
}

fn scope_name(scope: InstallScope) -> &'static str {
    match scope {
        InstallScope::Global => "global",
        InstallScope::Project => "project",
    }
}
