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
    let executable = resolve_powershell_with(
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        std::env::var_os("SystemRoot").map(PathBuf::from),
        find_executable_on_path,
        shell_path_exists,
    )
    .ok_or_else(|| {
        execution(
            "No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.",
        )
    })?;
    Ok(shell_configuration(executable))
}

fn resolve_powershell_with(
    program_files: Option<PathBuf>,
    system_root: Option<PathBuf>,
    mut find_on_path: impl FnMut(&str) -> Option<PathBuf>,
    mut path_exists: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(path) = find_on_path("pwsh.exe")
        && path_exists(&path)
    {
        return Some(path);
    }
    if let Some(path) = program_files.map(|root| root.join("PowerShell").join("7").join("pwsh.exe"))
        && path_exists(&path)
    {
        return Some(path);
    }
    if let Some(path) = find_on_path("powershell.exe")
        && path_exists(&path)
    {
        return Some(path);
    }
    if let Some(path) = system_root.map(|root| {
        root.join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe")
    }) && path_exists(&path)
    {
        return Some(path);
    }
    None
}

fn shell_path_exists(path: &Path) -> bool {
    if cfg!(windows) {
        std::fs::symlink_metadata(path).is_ok()
    } else {
        std::fs::metadata(path).is_ok()
    }
}

fn find_executable_on_path(executable: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        let where_executable = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .map(|root| root.join("System32").join("where.exe"))
            .filter(|path| shell_path_exists(path))
            .unwrap_or_else(|| PathBuf::from("where"));
        let output = std::process::Command::new(where_executable)
            .arg(executable)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let decoded = String::from_utf8_lossy(&output.stdout);
        let first = decoded
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        let path = PathBuf::from(first);
        return shell_path_exists(&path).then_some(path);
    }

    std::env::var_os("PATH").and_then(|value| {
        std::env::split_paths(&value)
            .map(|directory| directory.join(executable))
            .find(|path| shell_path_exists(path))
    })
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

    #[test]
    fn powershell_resolution_matches_path_and_known_location_precedence() {
        let program_files = PathBuf::from(r"D:\Applications");
        let system_root = PathBuf::from(r"E:\WinNT");
        let pwsh7 = program_files.join("PowerShell").join("7").join("pwsh.exe");
        let system_powershell = system_root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");

        let path_pwsh = PathBuf::from(r"E:\Tools\pwsh.exe");
        assert_eq!(
            resolve_powershell_with(
                Some(program_files.clone()),
                Some(system_root.clone()),
                |name| (name == "pwsh.exe").then(|| path_pwsh.clone()),
                |_| true,
            ),
            Some(path_pwsh)
        );

        let path_windows = PathBuf::from(r"E:\Tools\powershell.exe");
        assert_eq!(
            resolve_powershell_with(
                Some(program_files.clone()),
                Some(system_root.clone()),
                |name| (name == "powershell.exe").then(|| path_windows.clone()),
                |path| path == pwsh7 || path == system_powershell,
            ),
            Some(pwsh7.clone())
        );

        assert_eq!(
            resolve_powershell_with(
                None,
                Some(system_root.clone()),
                |name| (name == "powershell.exe").then(|| path_windows.clone()),
                |path| path == path_windows || path == system_powershell,
            ),
            Some(path_windows)
        );

        assert_eq!(
            resolve_powershell_with(
                None,
                Some(system_root.clone()),
                |_| None,
                |path| path == system_powershell,
            ),
            Some(system_powershell.clone())
        );

        let missing_alias = PathBuf::from(r"D:\WindowsApps\pwsh.exe");
        assert_eq!(
            resolve_powershell_with(
                None,
                Some(system_root),
                |name| (name == "pwsh.exe").then(|| missing_alias.clone()),
                |path| path == system_powershell,
            ),
            Some(system_powershell)
        );
        assert_eq!(
            resolve_powershell_with(None, None, |_| None, |_| false),
            None
        );
    }

    #[test]
    fn powershell_path_lookup_may_return_a_windows_store_alias() {
        let alias = PathBuf::from(r"D:\Users\test\AppData\Local\Microsoft\WindowsApps\pwsh.exe");
        assert_eq!(
            resolve_powershell_with(
                None,
                None,
                |name| (name == "pwsh.exe").then(|| alias.clone()),
                |path| path == alias,
            ),
            Some(alias)
        );
    }

    #[cfg(windows)]
    async fn run_with_powershell(executable: &Path, command: &str) -> pi_shell::ShellResult {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = pi_core::AbortHandle::new();
        pi_shell::execute_configured(
            pi_shell::ShellRequest {
                command: prepare_command(executable, command),
                cwd: directory.path().into(),
                timeout: None,
                shell_path: None,
                abort_signal: signal,
                on_chunk: None,
            },
            std::collections::BTreeMap::new(),
            shell_configuration(executable.into()),
        )
        .await
        .unwrap()
    }

    #[cfg(windows)]
    fn installed_powershells() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(path) = find_executable_on_path("pwsh.exe") {
            candidates.push(path);
        }
        if let Some(root) = std::env::var_os("ProgramFiles") {
            let path = PathBuf::from(root)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe");
            if shell_path_exists(&path) && !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        if let Some(path) = find_executable_on_path("powershell.exe") {
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        if let Some(root) = std::env::var_os("SystemRoot") {
            let path = PathBuf::from(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe");
            if shell_path_exists(&path) && !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        candidates
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn executes_powershell_commands_with_utf8_output() {
        let directory = tempfile::tempdir().unwrap();
        let (_, signal) = pi_core::AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = PowerShellTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("powershell-utf8"),
                serde_json::json!({
                    "command":"Write-Output 'héllo €'; Get-ExecutionPolicy -Scope Process"
                }),
                updates,
            )
            .await
            .unwrap();
        let pi_core::ContentBlock::Text(output) = &result.content[0] else {
            panic!("expected text output")
        };
        assert!(output.text.contains("héllo €"));
        assert!(output.text.contains("Bypass"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_exit_codes_match_powershell_seven() {
        let stderr_exit_zero = r#"cmd /c "echo progress 1>&2 & exit 0""#;
        let stderr_exit_seven = r#"cmd /c "echo progress 1>&2 & exit 7""#;
        let cases = [
            (
                format!("{stderr_exit_zero} 2>&1 | Select-Object -Last 3"),
                0,
            ),
            (format!("{stderr_exit_zero} 2>&1"), 0),
            (format!("{stderr_exit_zero} 2>$null"), 0),
            (format!("$output = {stderr_exit_zero} 2>&1"), 0),
            (stderr_exit_zero.to_string(), 0),
            (
                format!("Get-Item C:\\pi-missing-8842; {stderr_exit_zero} 2>&1"),
                0,
            ),
            (
                format!("{stderr_exit_seven} 2>&1 | Select-Object -Last 3"),
                1,
            ),
            (stderr_exit_seven.to_string(), 1),
            (r#"cmd /c "exit 3""#.to_string(), 1),
            (r"Get-Item C:\pi-missing-8842".to_string(), 1),
            (
                format!("{stderr_exit_zero} 2>&1; Get-Item C:\\pi-missing-8842"),
                1,
            ),
            (r#"Write-Error "failed""#.to_string(), 1),
            (r#"throw "failed""#.to_string(), 1),
            (format!("{stderr_exit_seven}; Write-Output done"), 0),
            ("Write-Output done # comment".to_string(), 0),
            (
                format!("Set-StrictMode -Version Latest; {stderr_exit_zero} 2>&1"),
                0,
            ),
            ("exit 5".to_string(), 5),
        ];
        let unfinished = [
            "Write-Output done `",
            "Get-ChildItem |",
            "$true -and",
            "Write-Output a,",
            "$x =",
            "Write-Output (1 +",
            "Write-Output \"unclosed",
            "if ($true) {",
        ];
        let shells = installed_powershells();
        assert!(!shells.is_empty(), "Windows must provide PowerShell");
        for shell in shells {
            for (command, expected) in &cases {
                let result = run_with_powershell(&shell, command).await;
                assert_eq!(
                    result.exit_code,
                    Some(*expected),
                    "{}: {command}",
                    shell.display()
                );
            }
            for statement in unfinished {
                let command = format!("Write-Output (\"pi-\" + \"ran\"); {statement}");
                let result = run_with_powershell(&shell, &command).await;
                assert_eq!(result.exit_code, Some(1), "{}: {command}", shell.display());
                assert!(
                    !result.output.contains("pi-ran"),
                    "{}: {command}",
                    shell.display()
                );
            }
        }
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
