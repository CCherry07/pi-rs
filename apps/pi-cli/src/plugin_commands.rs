use std::io::IsTerminal;

use pi_plugin_manager::{InstallScope, PluginManager, PluginManagerOptions};
use pi_settings::SettingsManager;

use crate::config::{AppConfig, Cli, PluginCommand, PluginPublishCommand};

pub(crate) async fn run(
    cli: &Cli,
    config: &AppConfig,
    command: &PluginCommand,
    settings: &SettingsManager,
) -> Result<(), String> {
    if is_author_command(command) {
        let command = command.clone();
        let cwd = config.cwd.clone();
        let output = tokio::task::spawn_blocking(move || run_author(&cwd, &command))
            .await
            .map_err(|e| e.to_string())??;
        println!("{output}");
        return Ok(());
    }
    let local = match command {
        PluginCommand::Install { local, .. }
        | PluginCommand::List { local }
        | PluginCommand::Sync { local, .. }
        | PluginCommand::Remove { local, .. } => *local,
        _ => unreachable!("author commands were handled above"),
    };
    if local {
        let trust = crate::resolve_project_trust(
            cli,
            config,
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            settings,
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
        _ => {}
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
        _ => unreachable!("author commands were handled above"),
    }
    Ok(())
}

fn is_author_command(command: &PluginCommand) -> bool {
    matches!(
        command,
        PluginCommand::New { .. }
            | PluginCommand::Package { .. }
            | PluginCommand::Verify { .. }
            | PluginCommand::Merge { .. }
            | PluginCommand::Publish { .. }
            | PluginCommand::RegistryEntry { .. }
    )
}

fn run_author(cwd: &std::path::Path, command: &PluginCommand) -> Result<String, String> {
    use pi_plugin_tools::{NewOptions, PackageOptions, SdkSource};
    let path = |value: &std::path::Path| {
        if value.is_absolute() {
            value.to_path_buf()
        } else {
            cwd.join(value)
        }
    };
    let result = match command {
        PluginCommand::New {
            path: destination,
            name,
            kind,
            sdk,
            sdk_rev,
        } => {
            let destination = path(destination);
            let name = name
                .clone()
                .or_else(|| {
                    destination
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
                .ok_or_else(|| "destination needs a directory name or --name".to_string())?;
            let sdk = match sdk {
                Some(sdk) => SdkSource::Path(path(sdk)),
                None => SdkSource::GitRevision(
                    sdk_rev
                        .clone()
                        .unwrap_or_else(|| pi_plugin_tools::SOURCE_REVISION.into()),
                ),
            };
            pi_plugin_tools::new_plugin(&NewOptions {
                destination,
                name,
                kind: *kind,
                sdk,
            })
            .map(|directory| {
                format!(
                    "Created {} plugin at {}\nNext: pi --cwd {} plugin package",
                    kind.as_str(),
                    directory.display(),
                    directory.display()
                )
            })
        }
        PluginCommand::Package {
            manifest_path,
            output,
            target_dir,
            locked,
            debug,
        } => pi_plugin_tools::package(&PackageOptions {
            manifest_path: path(manifest_path),
            output: path(output),
            target_dir: target_dir.as_deref().map(path),
            locked: *locked,
            debug: *debug,
        })
        .map(|directory| {
            format!(
                "Packaged and host-verified {} at {}",
                pi_plugin_tools::HOST_TARGET,
                directory.display()
            )
        }),
        PluginCommand::Verify {
            path: bundle,
            integrity_only,
        } => pi_plugin_tools::verify(&path(bundle), *integrity_only).map(|report| {
            format!(
                "Verified {} {}: {} artifact checksum(s); {}",
                report.release.id,
                report.release.version,
                report.release.artifacts.len(),
                report.host_verified.map_or_else(
                    || "integrity only, no native compatibility check".to_string(),
                    |target| format!("host compatibility passed for {target}")
                )
            )
        }),
        PluginCommand::Merge { bundles, output } => pi_plugin_tools::merge(
            &bundles
                .iter()
                .map(|bundle| path(bundle))
                .collect::<Vec<_>>(),
            &path(output),
        )
        .map(|directory| {
            format!(
                "Merged {} bundles into {} (integrity checked; run verify on each native target)",
                bundles.len(),
                directory.display()
            )
        }),
        PluginCommand::Publish {
            destination:
                PluginPublishCommand::Github {
                    bundle,
                    repo,
                    tag,
                    draft,
                },
        } => pi_plugin_tools::publish_github(&path(bundle), repo, tag, *draft).map(|url| {
            format!(
                "{} {url}",
                if *draft { "Created draft" } else { "Published" }
            )
        }),
        PluginCommand::RegistryEntry {
            bundle,
            manifest_url,
        } => pi_plugin_tools::registry_entry(&path(bundle), manifest_url).and_then(|value| {
            serde_json::to_string_pretty(&value)
                .map_err(|e| pi_plugin_tools::Error::Invalid(e.to_string()))
        }),
        _ => unreachable!("only author commands reach this adapter"),
    };
    result.map_err(|e| e.to_string())
}

fn scope_name(scope: InstallScope) -> &'static str {
    match scope {
        InstallScope::Global => "global",
        InstallScope::Project => "project",
    }
}
