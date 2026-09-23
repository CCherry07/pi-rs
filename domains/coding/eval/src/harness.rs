use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_coding::{Config, Pi};
use pi_eval::{ArtifactStore, EvalError, EvalRun, EvalRunContext, EvalRunner, PreparedEvalTarget};
use pi_js_plugin::JsPluginHost;
use pi_plugin::PresentationMode;

use crate::{CodingEvalCase, CodingEvalOptions, CodingEvalSystemPrompt, CodingEvalVariant};

/// Runs the Coding product in an isolated workspace through the generic eval runner.
pub struct CodingEvalHarness {
    runner: EvalRunner,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
}

impl CodingEvalHarness {
    pub fn new(artifacts: ArtifactStore) -> Self {
        Self {
            runner: EvalRunner::new(artifacts).ignored_directories([
                ".git",
                "node_modules",
                "target",
            ]),
            js_plugin_host: None,
        }
    }

    pub fn with_js_plugin_host(mut self, host: Arc<dyn JsPluginHost>) -> Self {
        self.js_plugin_host = Some(host);
        self
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        self.runner.artifacts()
    }

    pub async fn run(
        &self,
        case: &CodingEvalCase,
        variant: impl Into<CodingEvalVariant>,
        repetition: u32,
        config: Config,
    ) -> Result<EvalRun, EvalError> {
        let variant = variant.into();
        let options = case.options;
        let js_plugin_host = self.js_plugin_host.clone();
        self.runner
            .run(
                &case.case,
                variant.name.clone(),
                repetition,
                move |context| async move {
                    prepare_target(context, options, variant, config, js_plugin_host)
                },
            )
            .await
    }
}

fn prepare_target(
    context: EvalRunContext,
    options: CodingEvalOptions,
    variant: CodingEvalVariant,
    mut config: Config,
    js_plugin_host: Option<Arc<dyn JsPluginHost>>,
) -> Result<PreparedEvalTarget, EvalError> {
    if (options.requires_js_host || !variant.extensions.is_empty()) && js_plugin_host.is_none() {
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
    let source_cwd = config.cwd.clone();
    let isolated_home = context.root.join("home");
    let agent_dir = isolated_home.join(".pi/agent");
    std::fs::create_dir_all(&agent_dir)
        .map_err(|error| EvalError::Fixture(format!("cannot prepare eval directories: {error}")))?;
    for name in ["auth.json", "models.json"] {
        copy_bootstrap_file(&config.agent_dir, &agent_dir, name)?;
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
    if let Some(active_tools) = &context.active_tools {
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

    config.cwd = context.workspace.clone();
    config.agent_dir = agent_dir.clone();
    config.session_path = context.session_path;
    config.trust_override = Some(true);
    config.discover_extensions = options.discover_extensions;
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

    let mut builder = Pi::builder(config).presentation_mode(PresentationMode::Print);
    if let Some(host) = js_plugin_host {
        builder = builder.js_plugin_host(host);
    }
    let host = builder.build().map_err(EvalError::Runtime)?;
    let mut target = PreparedEvalTarget::new(host.session_manager(), provider, model);
    target.template_bindings = BTreeMap::from([
        (
            "workspace".to_string(),
            context.workspace.display().to_string(),
        ),
        ("agent_dir".to_string(), agent_dir.display().to_string()),
        ("home".to_string(), isolated_home.display().to_string()),
    ]);
    if variant.system_prompt == CodingEvalSystemPrompt::WithoutPiDocumentation {
        target.prompt_transform = Some(Arc::new(remove_pi_documentation));
    }
    Ok(target)
}

fn remove_pi_documentation(system_prompt: &str) -> Result<String, String> {
    let documentation_start = system_prompt
        .find("\nPi documentation (read only")
        .ok_or_else(|| "default system prompt has no Pi documentation section".to_string())?;
    // Coding appends configured instructions and project context after this final docs line.
    // Ending at the cwd marker would also remove those independent prompt contributions.
    const DOCUMENTATION_END: &str = "\n- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)";
    let documentation_end = system_prompt[documentation_start..]
        .find(DOCUMENTATION_END)
        .map(|offset| documentation_start + offset + DOCUMENTATION_END.len())
        .filter(|&end| system_prompt[end..].starts_with('\n'))
        .ok_or_else(|| "default system prompt has no Pi documentation end marker".to_string())?;
    if !system_prompt[documentation_end..].contains("\nCurrent working directory:") {
        return Err(
            "default system prompt has no current-working-directory marker after Pi documentation"
                .to_string(),
        );
    }
    Ok(format!(
        "{}{}",
        &system_prompt[..documentation_start],
        &system_prompt[documentation_end..]
    ))
}

fn copy_bootstrap_file(
    source_directory: &Path,
    destination_directory: &Path,
    name: &str,
) -> Result<(), EvalError> {
    let source = source_directory.join(name);
    if !source.exists() {
        return Ok(());
    }
    let destination = destination_directory.join(name);
    std::fs::copy(&source, &destination).map_err(|error| {
        EvalError::Fixture(format!(
            "cannot copy bootstrap file {} to {}: {error}",
            source.display(),
            destination.display()
        ))
    })?;
    Ok(())
}

fn resolve_local_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn resolve_extension_source(cwd: &Path, source: &str) -> String {
    let path = Path::new(source);
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

#[cfg(not(windows))]
fn isolated_shell_prefix(home: &Path) -> String {
    let quoted = format!("'{}'", home.display().to_string().replace('\'', "'\"'\"'"));
    format!(
        "export HOME={quoted}; unset PI_AGENT_DIR PI_CODING_AGENT_DIR PI_EVAL_ARTIFACT_DIR PI_MODEL PI_PROVIDER PI_REASONING_LEVEL PI_SESSION_FILE PI_SESSION_ID;"
    )
}

#[cfg(windows)]
fn isolated_shell_prefix(home: &Path) -> String {
    format!(
        "set \"HOME={}\" && set PI_AGENT_DIR= && set PI_CODING_AGENT_DIR= && set PI_EVAL_ARTIFACT_DIR= && set PI_MODEL= && set PI_PROVIDER= && set PI_REASONING_LEVEL= && set PI_SESSION_FILE= && set PI_SESSION_ID= &&",
        home.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_removal_preserves_append_and_project_context() {
        let docs = "\nPi documentation (read only):\n- Main documentation: README.md\n- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)";
        let suffix = "\n\nAppend instructions\n\n<project_context>Keep this context</project_context>\nCurrent working directory: /tmp";
        assert_eq!(
            remove_pi_documentation(&format!("before{docs}{suffix}")).unwrap(),
            format!("before{suffix}")
        );
        assert!(remove_pi_documentation("before").is_err());
        assert!(
            remove_pi_documentation(
                "before\nPi documentation (read only):\nCurrent working directory: /tmp"
            )
            .is_err()
        );
        assert!(remove_pi_documentation(&format!("before{docs}\n\nappend")).is_err());
    }

    #[test]
    fn extension_paths_use_the_original_working_directory() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("local.ts"),
            "export default () => {};",
        )
        .unwrap();
        for source in ["local.ts", "./missing.ts"] {
            assert_eq!(
                resolve_extension_source(directory.path(), source),
                directory.path().join(source).display().to_string()
            );
        }
        assert_eq!(
            resolve_extension_source(directory.path(), "npm:example-extension"),
            "npm:example-extension"
        );
    }

    #[test]
    fn shell_prefix_isolates_home_and_eval_control_variables() {
        let prefix = isolated_shell_prefix(Path::new("/tmp/pi eval"));
        assert!(prefix.contains("HOME="));
        for name in [
            "PI_AGENT_DIR",
            "PI_CODING_AGENT_DIR",
            "PI_EVAL_ARTIFACT_DIR",
            "PI_MODEL",
            "PI_PROVIDER",
            "PI_REASONING_LEVEL",
            "PI_SESSION_FILE",
            "PI_SESSION_ID",
        ] {
            assert!(prefix.contains(name), "missing {name}");
        }
    }
}
