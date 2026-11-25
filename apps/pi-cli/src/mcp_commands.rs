use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use pi_plugin_mcp::{
    McpInspection, McpLibrary, McpScope, McpServerDefinition, McpServerState, McpTransport,
};

const HELP: &str = "Usage:
  pi mcp add <server> [options] -- <command> [args...]
  pi mcp add <server> [options] --url <url>
  pi mcp remove <server> [-l]
  pi mcp list [--json]
  pi mcp login <server> [-l] [--timeout <seconds>]
  pi mcp logout <server> [-l]

Configure and check MCP servers without starting a session.
Reads <agent-dir>/mcp.json and, in trusted projects, .pi/mcp.json.

Commands:
  add <server>            Add or replace a server in mcp.json
  remove <server>         Remove a server from mcp.json
  list                    Show state, tools, and errors (exits 1 on failure)
  login <server>          Browser OAuth sign-in; terminal input enables pasted fallback
  logout <server>         Delete stored OAuth credentials for this server

Options for add and remove:
  -l, --local             Use .pi/mcp.json in the current project

Options for add:
  --url <url>             Streamable HTTP server URL (instead of a command)
  --env <KEY=VALUE>       Environment variable for a stdio server (repeatable)
  --cwd <dir>             Working directory for a stdio server
  --header <KEY=VALUE>    HTTP header (repeatable)
  --bearer-token-env-var <NAME>
                          Send Authorization: Bearer ${NAME}
  --oauth-client-id <id>  Pre-registered OAuth client id
  --oauth-client-secret <secret>
                          OAuth client secret (stored unresolved)
  --oauth-callback-port <port>
                          Fixed OAuth callback port
  --oauth-client-name <name>
                          Client name for OAuth registration
  --oauth-client-registration <mode>
                          auto (default), cimd, or dcr
  --exposure <mode>       codemode (default), deferred, direct, or hidden
  --description <text>    What the server offers, shown in the system prompt

Other options:
  --timeout <seconds>     Whole-login timeout (default 300; login only)
  --json                  Print the list as JSON";

#[derive(Clone, Debug)]
pub(crate) struct McpCommandContext {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub project_trusted: bool,
}

#[derive(Clone, Copy)]
enum OptionKind {
    Flag,
    Value,
    List,
}

#[derive(Default)]
struct ParsedOptions {
    positional: Vec<String>,
    values: BTreeMap<String, String>,
    flags: BTreeSet<String>,
    lists: BTreeMap<String, Vec<String>>,
}

pub(crate) async fn run(arguments: &[String], context: &McpCommandContext) -> Result<i32, String> {
    let Some((command, rest)) = arguments.split_first() else {
        println!("{HELP}");
        return Ok(0);
    };
    if matches!(command.as_str(), "help" | "--help" | "-h")
        || arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("{HELP}");
        return Ok(0);
    }
    let library = McpLibrary::new(
        &context.agent_dir,
        Some(&context.cwd),
        context.project_trusted,
    )
    .with_command_values(true)
    .with_client_auth(pi_coding::mcp_client_auth(&context.agent_dir))
    .with_oauth_host(std::sync::Arc::new(
        crate::mcp_oauth::NativeMcpOAuthHost::default(),
    ));
    match command.as_str() {
        "add" => add(rest, context, &library),
        "remove" => remove(rest, context, &library),
        "list" => list(rest, context, &library).await,
        "login" | "logout" => authorize(command, rest, &library).await,
        _ => {
            eprintln!("Unknown mcp command {command:?}.\nUse \"pi mcp --help\" for usage.");
            Ok(1)
        }
    }
}

async fn authorize(
    command: &str,
    arguments: &[String],
    library: &McpLibrary,
) -> Result<i32, String> {
    let mut known = BTreeMap::from([("local", OptionKind::Flag)]);
    if command == "login" {
        known.insert("timeout", OptionKind::Value);
    }
    let parsed = match parse_options(arguments, &known, usize::MAX) {
        Ok(parsed) => parsed,
        Err(error) => return command_error(error),
    };
    if parsed.positional.len() != 1 {
        return command_error(format!("Usage: pi mcp {command} <server> [-l]"));
    }
    let scope = parsed.flags.contains("local").then_some(McpScope::Project);
    let name = &parsed.positional[0];
    let result = if command == "login" {
        let timeout = match login_timeout(parsed.values.get("timeout").map(String::as_str)) {
            Ok(timeout) => timeout,
            Err(error) => return command_error(error),
        };
        login(library, name, scope, timeout).await
    } else {
        library.logout(name, scope)
    };
    match result {
        Ok(message) => {
            println!("{message}");
            Ok(0)
        }
        Err(error) => command_error(error),
    }
}

fn login_timeout(value: Option<&str>) -> Result<std::time::Duration, String> {
    let invalid = || {
        "--timeout must be a positive finite number of seconds representable as a duration"
            .to_string()
    };
    let seconds = value
        .unwrap_or("300")
        .parse::<f64>()
        .map_err(|_| invalid())?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(invalid());
    }
    std::time::Duration::try_from_secs_f64(seconds).map_err(|_| invalid())
}

async fn login(
    library: &McpLibrary,
    name: &str,
    scope: Option<McpScope>,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let (abort, signal) = pi_core::AbortHandle::new();
    let count = tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => { abort.abort(); return Err("MCP OAuth sign-in cancelled".into()); }
        result = tokio::time::timeout(timeout, library.login(name, scope, std::sync::Arc::new(crate::mcp_oauth::ShellOAuthPrompt), signal)) => {
            result.map_err(|_| "MCP login timed out; retry pi mcp login".to_string())?.map_err(|error| error.to_string())?
        }
    };
    Ok(format!("Signed in to MCP server {name:?} ({count} tools)."))
}

fn add(
    arguments: &[String],
    context: &McpCommandContext,
    library: &McpLibrary,
) -> Result<i32, String> {
    let known = BTreeMap::from([
        ("local", OptionKind::Flag),
        ("url", OptionKind::Value),
        ("oauth-client-id", OptionKind::Value),
        ("oauth-client-secret", OptionKind::Value),
        ("oauth-callback-port", OptionKind::Value),
        ("oauth-client-name", OptionKind::Value),
        ("oauth-client-registration", OptionKind::Value),
        ("env", OptionKind::List),
        ("cwd", OptionKind::Value),
        ("header", OptionKind::List),
        ("bearer-token-env-var", OptionKind::Value),
        ("exposure", OptionKind::Value),
        ("description", OptionKind::Value),
    ]);
    let parsed = match parse_options(arguments, &known, 2) {
        Ok(parsed) => parsed,
        Err(error) => return command_error(error),
    };
    let Some(name) = parsed.positional.first() else {
        return command_error(add_usage());
    };
    let command = &parsed.positional[1..];
    let url = parsed.values.get("url");
    if url.is_some() != command.is_empty() {
        return command_error(add_usage());
    }
    if url.is_some() && (parsed.lists.contains_key("env") || parsed.values.contains_key("cwd")) {
        return command_error("--env and --cwd only apply to stdio servers");
    }
    const HTTP_ONLY: &[&str] = &[
        "header",
        "bearer-token-env-var",
        "oauth-client-id",
        "oauth-client-secret",
        "oauth-callback-port",
        "oauth-client-name",
        "oauth-client-registration",
    ];
    if url.is_none()
        && let Some(option) = HTTP_ONLY
            .iter()
            .find(|name| parsed.values.contains_key(**name) || parsed.lists.contains_key(**name))
    {
        return command_error(format!("--{option} only applies to HTTP servers (--url)"));
    }

    let transport = if let Some(url) = url {
        let mut headers = match parse_pairs("header", parsed.lists.get("header")) {
            Ok(headers) => headers,
            Err(error) => return command_error(error),
        };
        if let Some(variable) = parsed.values.get("bearer-token-env-var") {
            if !valid_env_name(variable) {
                return command_error(
                    "--bearer-token-env-var needs a valid environment variable name",
                );
            }
            headers.insert("Authorization".into(), format!("Bearer ${{{variable}}}"));
        }
        McpTransport::Http {
            url: url.clone(),
            headers,
        }
    } else {
        let mut command = command.iter();
        let executable = command.next().expect("validated command").clone();
        let env = match parse_pairs("env", parsed.lists.get("env")) {
            Ok(env) => env,
            Err(error) => return command_error(error),
        };
        McpTransport::Stdio {
            command: executable,
            args: command.cloned().collect(),
            env,
            cwd: parsed.values.get("cwd").map(PathBuf::from),
        }
    };
    let mut definition = McpServerDefinition::new(name, transport);
    if HTTP_ONLY
        .iter()
        .filter(|name| name.starts_with("oauth-"))
        .any(|name| parsed.values.contains_key(*name))
    {
        let callback_port = match parsed.values.get("oauth-callback-port") {
            Some(value) => match value.parse::<u16>() {
                Ok(value @ 1..) => Some(value),
                _ => {
                    return command_error(
                        "--oauth-callback-port must be an integer from 1 to 65535",
                    );
                }
            },
            None => None,
        };
        let client_registration = match parsed.values.get("oauth-client-registration") {
            Some(value) => match serde_json::from_value(serde_json::json!(value)) {
                Ok(value) => Some(value),
                Err(_) => {
                    return command_error("--oauth-client-registration must be auto, cimd, or dcr");
                }
            },
            None => None,
        };
        definition = definition.oauth(pi_plugin_mcp::McpOAuthConfig {
            client_id: parsed.values.get("oauth-client-id").cloned(),
            client_secret: parsed.values.get("oauth-client-secret").cloned(),
            callback_port,
            client_name: parsed.values.get("oauth-client-name").cloned(),
            client_registration,
            ..Default::default()
        });
    }
    if let Some(exposure) = parsed.values.get("exposure") {
        let exposure = match exposure.parse::<pi_plugin_mcp::McpExposure>() {
            Ok(exposure) => exposure,
            Err(error) => return command_error(error),
        };
        definition = definition.exposure(exposure);
    }
    if let Some(description) = parsed.values.get("description") {
        definition = definition.description(description);
    }
    let local = parsed.flags.contains("local");
    let scope = if local {
        McpScope::Project
    } else {
        McpScope::Global
    };
    // Local authoring is allowed before trust, like current Pi. Use a trusted disk view only for
    // this explicit write; session loading continues to use the host's persisted trust decision.
    let write_library = if local {
        McpLibrary::new(&context.agent_dir, Some(&context.cwd), true)
    } else {
        library.clone()
    };
    let path = scoped_path(context, scope);
    match write_library.add(scope, &definition) {
        Ok(replaced) => {
            println!(
                "{} {} MCP server {:?} in {}.",
                if replaced { "Replaced" } else { "Added" },
                scope_name(scope),
                name,
                path.display()
            );
            if local && !context.project_trusted {
                println!(
                    "The project is not trusted, so {} is ignored until you start pi in the project and trust it.",
                    path.display()
                );
            }
            println!("Check it with: pi mcp list");
            Ok(0)
        }
        Err(error) => command_error(format!("Could not update {}: {error}", path.display())),
    }
}

fn remove(
    arguments: &[String],
    context: &McpCommandContext,
    library: &McpLibrary,
) -> Result<i32, String> {
    let known = BTreeMap::from([("local", OptionKind::Flag)]);
    let parsed = match parse_options(arguments, &known, usize::MAX) {
        Ok(parsed) => parsed,
        Err(error) => return command_error(error),
    };
    if parsed.positional.len() != 1 {
        return command_error("Usage: pi mcp remove <server> [-l]");
    }
    let name = &parsed.positional[0];
    let local = parsed.flags.contains("local");
    let scope = if local {
        McpScope::Project
    } else {
        McpScope::Global
    };
    let write_library = if local {
        McpLibrary::new(&context.agent_dir, Some(&context.cwd), true)
    } else {
        library.clone()
    };
    let path = scoped_path(context, scope);
    match write_library.remove(scope, name) {
        Ok(true) => {
            println!(
                "Removed {} MCP server {:?} from {}.",
                scope_name(scope),
                name,
                path.display()
            );
            Ok(0)
        }
        Ok(false) => {
            let other_scope = if local {
                McpScope::Global
            } else {
                McpScope::Project
            };
            let other = McpLibrary::new(&context.agent_dir, Some(&context.cwd), true)
                .contains_definition(other_scope, name)
                .unwrap_or(false);
            let hint = match (other, local) {
                (true, true) => "; it is defined globally, omit --local",
                (true, false) => "; it is defined in the project, use --local",
                (false, _) => "",
            };
            command_error(format!(
                "No {} MCP server named {:?} in {}{}.",
                scope_name(scope),
                name,
                path.display(),
                hint
            ))
        }
        Err(error) => command_error(format!("Could not update {}: {error}", path.display())),
    }
}

async fn list(
    arguments: &[String],
    context: &McpCommandContext,
    library: &McpLibrary,
) -> Result<i32, String> {
    let known = BTreeMap::from([("json", OptionKind::Flag)]);
    let parsed = match parse_options(arguments, &known, usize::MAX) {
        Ok(parsed) => parsed,
        Err(error) => return command_error(error),
    };
    if !parsed.positional.is_empty() {
        return command_error("Usage: pi mcp list [--json]");
    }
    let inspection = library.inspect().await;
    let project_path = context.cwd.join(".pi/mcp.json");
    let note = (!context.project_trusted && project_path.exists()).then(|| {
        format!(
            "{} is ignored because the project is not trusted. Start pi in the project to trust it.",
            project_path.display()
        )
    });
    if parsed.flags.contains("json") {
        let mut value = serde_json::json!({
            "servers": inspection.servers,
            "errors": inspection.errors,
        });
        if let Some(note) = &note {
            value["note"] = serde_json::json!(note);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|error| format!("Could not encode MCP status: {error}"))?
        );
    } else {
        render_text(&inspection, context, note.as_deref());
    }
    Ok(if inspection.is_ok() { 0 } else { 1 })
}

fn render_text(inspection: &McpInspection, context: &McpCommandContext, note: Option<&str>) {
    if inspection.servers.is_empty() && inspection.errors.is_empty() {
        println!(
            "No MCP servers configured. Add them to {} or .pi/mcp.json.",
            context.agent_dir.join("mcp.json").display()
        );
    }
    for report in &inspection.servers {
        let state = match report.state {
            McpServerState::Connected => format!(
                "connected, {} tool{}",
                report.tools.len(),
                if report.tools.len() == 1 { "" } else { "s" }
            ),
            McpServerState::NeedsAuth => "needs sign-in".into(),
            McpServerState::Disabled => "disabled".into(),
            McpServerState::Failed => "failed".into(),
        };
        println!(
            "{}: {} ({}, {})",
            report.name,
            state,
            report.exposure.as_str(),
            scope_name(report.scope)
        );
        println!("  {}", report.transport);
        if let Some(path) = &report.project_override {
            println!("  project override: {}", path.display());
        }
        if !report.tools.is_empty() {
            let tools = report
                .tools
                .iter()
                .map(|name| {
                    report.tool_exposure.get(name).map_or_else(
                        || name.clone(),
                        |mode| format!("{name} [{}]", mode.as_str()),
                    )
                })
                .collect::<Vec<_>>();
            println!("  tools: {}", tools.join(", "));
        }
        if report.resources {
            println!("  offers resources");
        }
        if let Some(error) = &report.error {
            println!("  {}", error.replace('\n', "\n  "));
        }
    }
    for error in &inspection.errors {
        println!("config error: {error}");
    }
    if let Some(note) = note {
        println!("{note}");
    }
}

fn parse_options(
    arguments: &[String],
    known: &BTreeMap<&str, OptionKind>,
    max_positionals: usize,
) -> Result<ParsedOptions, String> {
    let mut parsed = ParsedOptions::default();
    let mut index = 0;
    while index < arguments.len() {
        let typed = &arguments[index];
        let argument = if typed == "-l" { "--local" } else { typed };
        if argument == "--" || parsed.positional.len() >= max_positionals {
            let start = if argument == "--" { index + 1 } else { index };
            parsed.positional.extend_from_slice(&arguments[start..]);
            break;
        }
        if !argument.starts_with("--") {
            parsed.positional.push(argument.to_string());
            index += 1;
            continue;
        }
        let name = &argument[2..];
        let Some(kind) = known.get(name) else {
            return Err(format!(
                "Unknown option {typed}.\nUse \"pi mcp --help\" for usage."
            ));
        };
        match kind {
            OptionKind::Flag => {
                parsed.flags.insert(name.to_string());
            }
            OptionKind::Value | OptionKind::List => {
                index += 1;
                let Some(value) = arguments.get(index) else {
                    return Err(format!("{argument} needs a value."));
                };
                match kind {
                    OptionKind::Value => {
                        parsed.values.insert(name.to_string(), value.clone());
                    }
                    OptionKind::List => parsed
                        .lists
                        .entry(name.to_string())
                        .or_default()
                        .push(value.clone()),
                    OptionKind::Flag => unreachable!(),
                }
            }
        }
        index += 1;
    }
    Ok(parsed)
}

fn parse_pairs(
    option: &str,
    pairs: Option<&Vec<String>>,
) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    for pair in pairs.into_iter().flatten() {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!("--{option} expects KEY=VALUE, got {pair:?}."));
        };
        if key.is_empty() {
            return Err(format!("--{option} expects KEY=VALUE, got {pair:?}."));
        }
        values.insert(key.into(), value.into());
    }
    Ok(values)
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
}

fn add_usage() -> &'static str {
    "Usage: pi mcp add <server> [options] (--url <url> | -- <command> [args...])"
}

fn command_error(message: impl AsRef<str>) -> Result<i32, String> {
    eprintln!("{}", message.as_ref());
    Ok(1)
}

fn scope_name(scope: McpScope) -> &'static str {
    match scope {
        McpScope::Global => "global",
        McpScope::Project => "project",
    }
}

fn scoped_path(context: &McpCommandContext, scope: McpScope) -> PathBuf {
    match scope {
        McpScope::Global => context.agent_dir.join("mcp.json"),
        McpScope::Project => context.cwd.join(".pi/mcp.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn context(root: &Path) -> McpCommandContext {
        McpCommandContext {
            cwd: root.to_path_buf(),
            agent_dir: root.join("agent"),
            project_trusted: false,
        }
    }

    #[tokio::test]
    async fn add_remove_and_json_list_use_scoped_safe_library_writes() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(directory.path());
        let add = [
            "add",
            "--env",
            "A=1",
            "files",
            "--",
            "node",
            "server.js",
            "--port",
            "1",
        ]
        .map(String::from);
        assert_eq!(run(&add, &context).await.unwrap(), 0);
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(context.agent_dir.join("mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["mcpServers"]["files"]["command"], "node");
        assert_eq!(value["mcpServers"]["files"]["args"][2], "1");
        assert_eq!(value["mcpServers"]["files"]["env"]["A"], "1");

        let local = ["add", "--local", "parked", "--", "unused"].map(String::from);
        assert_eq!(run(&local, &context).await.unwrap(), 0);
        assert!(directory.path().join(".pi/mcp.json").exists());
        let remove = ["remove", "--local", "parked"].map(String::from);
        assert_eq!(run(&remove, &context).await.unwrap(), 0);
    }

    #[test]
    fn login_timeouts_accept_fractional_positive_seconds_without_panicking() {
        assert_eq!(
            login_timeout(None).unwrap(),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            login_timeout(Some("1.5")).unwrap(),
            std::time::Duration::from_millis(1500)
        );
        assert_eq!(
            login_timeout(Some("86401")).unwrap(),
            std::time::Duration::from_secs(86401)
        );
        for value in ["0", "-1", "NaN", "inf", "1e300", "oops"] {
            assert!(login_timeout(Some(value)).is_err(), "{value}");
        }
    }

    #[tokio::test]
    async fn invalid_add_does_not_create_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(directory.path());
        for arguments in [
            vec!["add", "x"],
            vec!["add", "bad name", "--", "cmd"],
            vec!["add", "x", "--url", "ftp://example.com"],
            vec!["add", "x", "--env", "NOVALUE", "--", "cmd"],
            vec!["add", "x", "--exposure", "loud", "--", "cmd"],
        ] {
            let arguments = arguments.into_iter().map(String::from).collect::<Vec<_>>();
            assert_eq!(run(&arguments, &context).await.unwrap(), 1);
            assert!(!context.agent_dir.join("mcp.json").exists());
        }
    }

    #[tokio::test]
    async fn list_exit_status_reflects_failures_without_starting_a_session() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(directory.path());
        std::fs::create_dir_all(&context.agent_dir).unwrap();
        std::fs::write(
            context.agent_dir.join("mcp.json"),
            r#"{"mcpServers":{"parked":{"command":"unused","enabled":false}}}"#,
        )
        .unwrap();
        assert_eq!(
            run(&["list".into(), "--json".into()], &context)
                .await
                .unwrap(),
            0
        );
        std::fs::write(context.agent_dir.join("mcp.json"), "broken").unwrap();
        assert_eq!(run(&["list".into()], &context).await.unwrap(), 1);
    }
}
