#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{PluginId, ToolCallId, ToolResult, ToolSpec};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_plugin_bash::{BashToolOptions, execute_shell_tool, shell_tool_spec};
use pi_shell::ShellConfiguration;
use pi_tool_support::{execution, require_str};
use serde_json::Value;

const UTF8_OUTPUT_PREFIX: &str =
    "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n";
const WINDOWS_POWERSHELL_EXIT_SUFFIX: &str = "\n;\nif ($?) { exit 0 }\nif (($LASTEXITCODE -eq 0) -and (\"$($Error[0].FullyQualifiedErrorId)\" -like \"NativeCommandError*\")) { exit 0 }\nexit 1";

pub struct PowerShellPlugin;
pub struct PowerShellTool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerShellToolOptions {
    pub expose_session_environment: bool,
}

impl Default for PowerShellToolOptions {
    fn default() -> Self {
        Self {
            expose_session_environment: true,
        }
    }
}

pub struct ConfiguredPowerShellPlugin {
    options: PowerShellToolOptions,
}

impl ConfiguredPowerShellPlugin {
    pub fn new(options: PowerShellToolOptions) -> Self {
        Self { options }
    }
}

struct ConfiguredPowerShellTool {
    options: PowerShellToolOptions,
}

#[pi_plugin::plugin]
impl Plugin for PowerShellPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("powershell-tool")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(PowerShellTool))
    }
}

#[pi_plugin::plugin]
impl Plugin for ConfiguredPowerShellPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("powershell-tool")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(ConfiguredPowerShellTool {
            options: self.options.clone(),
        }))
    }
}

#[async_trait]
impl Tool for PowerShellTool {
    fn spec(&self) -> ToolSpec {
        powershell_spec(true)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        execute_powershell(&PowerShellToolOptions::default(), context, input, updates).await
    }
}

#[async_trait]
impl Tool for ConfiguredPowerShellTool {
    fn spec(&self) -> ToolSpec {
        powershell_spec(self.options.expose_session_environment)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        execute_powershell(&self.options, context, input, updates).await
    }
}

fn powershell_spec(expose_session_environment: bool) -> ToolSpec {
    shell_tool_spec(
        "powershell",
        "PowerShell",
        "Execute PowerShell commands",
        expose_session_environment,
    )
}

async fn execute_powershell(
    options: &PowerShellToolOptions,
    context: ToolContext,
    mut input: Value,
    updates: ToolUpdateSink,
) -> Result<ToolResult, ToolError> {
    let command = require_str(&input, "command")?;
    let configuration = resolve_powershell()?;
    input["command"] = Value::String(prepare_command(&configuration.executable, command));
    let options = BashToolOptions {
        expose_session_environment: options.expose_session_environment,
        ..Default::default()
    };
    execute_shell_tool(&options, Some(configuration), context, input, updates).await
}

fn prepare_command(executable: &Path, command: &str) -> String {
    // A final backtick continues the user's line, so do not attach another statement.
    let windows_powershell = executable.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .eq_ignore_ascii_case("powershell.exe")
    });
    let suffix = if windows_powershell && !command.ends_with('`') {
        WINDOWS_POWERSHELL_EXIT_SUFFIX
    } else {
        ""
    };
    format!("{UTF8_OUTPUT_PREFIX}{command}{suffix}")
}

fn shell_configuration(executable: PathBuf) -> ShellConfiguration {
    ShellConfiguration {
        executable,
        arguments: [
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        output_file_prefix: "pi-powershell-".into(),
    }
}

fn resolve_powershell() -> Result<ShellConfiguration, ToolError> {
    if !cfg!(windows) {
        return Err(execution(
            "The powershell tool is only available on Windows.",
        ));
    }
    let on_path = |name: &str| {
        std::env::var_os("PATH").and_then(|value| {
            std::env::split_paths(&value)
                .map(|directory| directory.join(name))
                .find(|path| path.is_file())
        })
    };
    let executable = on_path("pwsh.exe")
        .or_else(|| std::env::var_os("ProgramFiles").map(|path| PathBuf::from(path).join("PowerShell/7/pwsh.exe")).filter(|path| path.is_file()))
        .or_else(|| on_path("powershell.exe"))
        .or_else(|| std::env::var_os("SystemRoot").map(|path| PathBuf::from(path).join("System32/WindowsPowerShell/v1.0/powershell.exe")).filter(|path| path.is_file()))
        .ok_or_else(|| execution("No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH."))?;
    Ok(shell_configuration(executable))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(windows))]
    use serde_json::json;

    #[test]
    fn windows_powershell_repairs_native_stderr_exit_status_only_for_complete_commands() {
        let legacy = prepare_command(Path::new("powershell.exe"), "git status 2>&1");
        assert!(legacy.starts_with(UTF8_OUTPUT_PREFIX));
        assert!(legacy.ends_with(WINDOWS_POWERSHELL_EXIT_SUFFIX));
        assert_eq!(
            prepare_command(Path::new("powershell.exe"), "Write-Output `"),
            format!("{UTF8_OUTPUT_PREFIX}Write-Output `")
        );
        assert_eq!(
            prepare_command(Path::new("pwsh.exe"), "Write-Output ok"),
            format!("{UTF8_OUTPUT_PREFIX}Write-Output ok")
        );
    }

    #[test]
    fn declaration_uses_shell_contract_and_configurable_environment_guidance() {
        let spec = PowerShellTool.spec();
        assert_eq!(spec.name, "powershell");
        assert_eq!(
            spec.prompt_snippet.as_deref(),
            Some("Execute PowerShell commands")
        );
        assert_eq!(spec.parameters["properties"]["timeout"]["type"], "number");
        assert!(
            spec.output_schema.unwrap()["properties"]
                .get("exit_code")
                .is_some()
        );
        assert_eq!(
            spec.constrained_sampling,
            Some(pi_core::ConstrainedSampling::JsonSchema {
                strict: pi_core::ToolSchemaStrict::Prefer
            })
        );
        assert!(powershell_spec(false).prompt_guidelines.is_empty());
    }

    #[test]
    fn process_arguments_match_current_pi() {
        let configuration = shell_configuration("pwsh.exe".into());
        assert_eq!(
            configuration.arguments,
            [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command"
            ]
        );
        assert_eq!(configuration.output_file_prefix, "pi-powershell-");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn execution_reports_windows_requirement() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = pi_core::AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let error = PowerShellTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("powershell"),
                json!({"command":"Write-Output ok"}),
                updates,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("only available on Windows"));
    }
}
