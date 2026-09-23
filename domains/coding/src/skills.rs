//! Read-only product configuration for the Desktop skill-file management seam.
//! Does not initialize providers, sessions, native plugins or background memory work.
use std::path::Path;

use pi_memory_loader::MemoryLoader;
use pi_plugin_memory_hermes::managed_skill_roots;
use pi_plugin_skills::{SkillLoaderOptions, management::SkillLibrary};
use pi_settings::{SettingsContext, SettingsManager};

use crate::{Config, ProjectTrustEvaluation, ProjectTrustService};

pub(crate) fn runtime_skill_options(
    config: &Config,
    trusted: bool,
    hermes: bool,
) -> SkillLoaderOptions {
    let mut options = SkillLoaderOptions::new(&config.cwd, &config.agent_dir);
    options.project_trusted = trusted;
    options.enable_commands = config.runtime_settings.enable_skill_commands;
    options
        .additional_paths
        .extend(config.settings_skill_paths.iter().cloned());
    if hermes {
        options.additional_paths.extend(managed_skill_roots(
            &config.agent_dir,
            &config.cwd,
            trusted,
        ));
    }
    options
}

/// Builds a fresh disk view using Desktop's native-only resource configuration.
/// Unknown trust is treated as untrusted; this read never opens a trust dialog.
pub fn desktop_skill_library(
    agent_dir: &Path,
    cwd: Option<&Path>,
    trust: &ProjectTrustService,
) -> Result<(SkillLibrary, bool), String> {
    let trusted = match cwd {
        Some(cwd) => matches!(
            trust.evaluate(cwd).map_err(|e| e.to_string())?,
            ProjectTrustEvaluation::Known(true)
        ),
        None => false,
    };
    let base = cwd.unwrap_or(agent_dir);
    let settings = SettingsManager::new(agent_dir).load(&SettingsContext::new(base, trusted));
    let mut config = Config::new(base.to_path_buf(), agent_dir.to_path_buf());
    config.runtime_settings = settings.effective().clone();
    config.settings_skill_paths = crate::configuration::scoped_setting_paths(
        &settings.global().skills,
        agent_dir,
        &settings.project().skills,
        &base.join(".pi"),
    );
    config.discover_extensions = false;
    config.settings_skill_paths.extend(
        pi_js_package_manager::PackageManager::new(config.javascript_resolve_request(trusted))
            .installed_skill_paths(),
    );
    let hermes = MemoryLoader::selected_provider(agent_dir)
        .map_err(|e| e.to_string())?
        .as_deref()
        == Some("hermes");
    let options = runtime_skill_options(&config, trusted, hermes);
    let destination = match cwd {
        Some(cwd) if trusted => Some(cwd.join(".pi/skills")),
        Some(_) => None,
        None => Some(agent_dir.join("skills")),
    };
    Ok((SkillLibrary::new(options, destination), trusted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_options_only_add_configured_and_cross_feature_roots() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        let agent_dir = dir.path().join("agent");
        let mut config = Config::new(cwd.clone(), agent_dir.clone());
        config
            .settings_skill_paths
            .push(cwd.join("configured-skills"));
        config.runtime_settings.enable_skill_commands = false;

        let options = runtime_skill_options(&config, false, false);
        assert!(options.include_defaults);
        assert!(!options.project_trusted);
        assert!(!options.enable_commands);
        assert_eq!(options.additional_paths, config.settings_skill_paths);

        let mut expected = config.settings_skill_paths.clone();
        expected.extend(managed_skill_roots(&agent_dir, &cwd, false));
        assert_eq!(
            runtime_skill_options(&config, false, true).additional_paths,
            expected
        );
    }

    #[test]
    fn disk_view_gates_projects_and_never_initializes_memory() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let agent = root.join("agent");
        let project = root.join("project");
        std::fs::create_dir_all(project.join(".pi/skills/check")).unwrap();
        std::fs::write(
            project.join(".pi/skills/check/SKILL.md"),
            "---\nname: check\ndescription: Check\n---\nBody",
        )
        .unwrap();
        let (trust, _) =
            ProjectTrustService::new(&agent, None, false, pi_settings::DefaultProjectTrust::Ask)
                .unwrap();
        let (library, trusted) = desktop_skill_library(&agent, Some(&project), &trust).unwrap();
        assert!(!trusted);
        assert!(library.destination().is_none());
        assert!(
            !library
                .list()
                .iter()
                .any(|row| row.path.starts_with(&project))
        );
        assert!(!agent.join("pi-hermes-memory").exists());
        trust.remember(&project, true).unwrap();
        let (library, trusted) = desktop_skill_library(&agent, Some(&project), &trust).unwrap();
        assert!(trusted);
        assert!(library.list().iter().any(|row| row.name == "check"));
        assert!(!agent.join("pi-hermes-memory").exists());
    }
}
