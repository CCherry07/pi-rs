//! Product-owned MCP configuration. Disk views never initialize a session or connect.
//! This is a deliberate Rust product extension; upstream Pi has no built-in MCP.
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel,
    PluginId, RegisterContext,
};
use pi_mcp::{McpServerConfig, McpToolSet, McpTransport};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{ProjectTrustEvaluation, ProjectTrustService};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const EMPTY: &str = "{\n  \"version\": 1,\n  \"mcpServers\": {}\n}\n";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpScope {
    Global,
    Project,
}

#[derive(Clone)]
pub struct McpLibrary {
    agent_dir: PathBuf,
    cwd: Option<PathBuf>,
    trusted: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDocument {
    pub path: PathBuf,
    pub content: String,
    pub revision: String,
    pub writable: bool,
    pub project_trusted: bool,
    pub diagnostic: Option<String>,
    pub servers: Vec<McpServerRow>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerRow {
    pub name: String,
    pub scope: McpScope,
    pub transport: String,
    pub enabled: bool,
}

struct Entry {
    name: String,
    scope: McpScope,
    transport: Option<McpTransport>,
    enabled: bool,
}

impl McpLibrary {
    pub fn new(agent_dir: &Path, cwd: Option<&Path>, trusted: bool) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            cwd: cwd.map(Path::to_path_buf),
            trusted,
        }
    }

    pub fn for_desktop(
        agent_dir: &Path,
        cwd: Option<&Path>,
        trust: &ProjectTrustService,
    ) -> Result<Self, String> {
        let trusted = match cwd {
            Some(cwd) => matches!(
                trust.evaluate(cwd).map_err(|e| e.to_string())?,
                ProjectTrustEvaluation::Known(true)
            ),
            None => false,
        };
        Ok(Self::new(agent_dir, cwd, trusted))
    }

    fn path(&self, scope: McpScope) -> Result<PathBuf, String> {
        match scope {
            McpScope::Global => Ok(self.agent_dir.join("mcp.json")),
            McpScope::Project if !self.trusted => {
                Err("Project MCP configuration requires project trust".into())
            }
            McpScope::Project => self
                .cwd
                .as_ref()
                .map(|cwd| cwd.join(".pi/mcp.json"))
                .ok_or_else(|| "Select a project first".into()),
        }
    }

    fn entries(&self) -> Result<Vec<Entry>, String> {
        let mut merged = BTreeMap::new();
        for scope in [McpScope::Global, McpScope::Project] {
            if scope == McpScope::Project && (!self.trusted || self.cwd.is_none()) {
                continue;
            }
            let path = self.path(scope)?;
            let content = read_document(&path)?.unwrap_or_else(|| EMPTY.into());
            for entry in parse(&content, scope)? {
                merged.insert(entry.name.clone(), entry);
            }
        }
        Ok(merged.into_values().collect())
    }

    pub fn read(&self, scope: McpScope) -> Result<McpDocument, String> {
        // Never even read project bytes before trust is granted.
        let path = self.path(scope)?;
        let bytes = read_document(&path)?;
        let revision = revision(bytes.as_deref());
        let content = bytes.unwrap_or_else(|| EMPTY.into());
        let (servers, diagnostic) = match self.entries() {
            Ok(entries) => (entries.iter().map(Entry::row).collect(), None),
            Err(error) => (Vec::new(), Some(error)),
        };
        Ok(McpDocument {
            path,
            content,
            revision,
            writable: true,
            project_trusted: self.trusted,
            diagnostic,
            servers,
        })
    }

    pub fn save(
        &self,
        scope: McpScope,
        expected_revision: &str,
        content: &str,
    ) -> Result<McpDocument, String> {
        let path = self.path(scope)?;
        parse(content, scope)?;
        let parent = path.parent().ok_or("Invalid MCP configuration path")?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(parent.join(".mcp.json.lock"))
            .map_err(io_error)?;
        fs2::FileExt::lock_exclusive(&lock).map_err(io_error)?;
        if revision(read_document(&path)?.as_deref()) != expected_revision {
            return Err("MCP configuration changed on disk; reload before saving".into());
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        temp.write_all(content.as_bytes()).map_err(io_error)?;
        temp.as_file().sync_all().map_err(io_error)?;
        // Detect non-cooperating editors too, before the atomic replacement.
        if revision(read_document(&path)?.as_deref()) != expected_revision {
            return Err("MCP configuration changed on disk; reload before saving".into());
        }
        temp.persist(&path).map_err(|e| io_error(e.error))?;
        self.read(scope)
    }

    pub async fn test(&self, name: &str) -> Result<Vec<String>, String> {
        let entry = self
            .entries()?
            .into_iter()
            .find(|entry| entry.name == name)
            .ok_or("MCP server not found")?;
        if !entry.enabled {
            return Err("Enable and save the server before testing".into());
        }
        let config = self.resolve(entry)?;
        let pool = McpToolSet::connect(vec![config])
            .await
            .map_err(|e| e.to_string())?;
        let names = pool.tools().into_iter().map(|tool| tool.name).collect();
        pool.shutdown().await.map_err(|e| e.to_string())?;
        Ok(names)
    }

    pub(crate) async fn prepare(&self) -> Result<Arc<dyn AgentPlugin>, String> {
        let entries = self.entries()?;
        let rows = entries.iter().map(Entry::row).collect();
        let configs = entries
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| self.resolve(entry))
            .collect::<Result<Vec<_>, _>>()?;
        let pool = McpToolSet::connect(configs)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(McpProductPlugin {
            library: self.clone(),
            rows,
            pool,
        }))
    }

    fn resolve(&self, entry: Entry) -> Result<McpServerConfig, String> {
        let mut transport = entry
            .transport
            .ok_or("Enabled MCP server needs a transport")?;
        match &mut transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                *command = crate::expand_tilde_path(&expand_env(command)?)
                    .to_string_lossy()
                    .into_owned();
                for arg in args {
                    *arg = expand_env(arg)?;
                }
                for value in env.values_mut() {
                    *value = expand_env(value)?;
                }
                *cwd = Some(match cwd.take() {
                    Some(path) => {
                        let path = crate::expand_tilde_path(&expand_env(&path.to_string_lossy())?);
                        if path.is_absolute() {
                            path
                        } else {
                            self.path(entry.scope)?.parent().unwrap().join(path)
                        }
                    }
                    None => self.cwd.as_ref().unwrap_or(&self.agent_dir).clone(),
                });
            }
            McpTransport::Http { url, headers } => {
                *url = expand_env(url)?;
                for value in headers.values_mut() {
                    *value = expand_env(value)?;
                }
            }
        }
        Ok(McpServerConfig {
            name: entry.name,
            transport,
        })
    }
}

impl Entry {
    fn row(&self) -> McpServerRow {
        McpServerRow {
            name: self.name.clone(),
            scope: self.scope,
            enabled: self.enabled,
            transport: match self.transport {
                Some(McpTransport::Http { .. }) => "http",
                Some(McpTransport::Stdio { .. }) => "stdio",
                None => "disabled",
            }
            .into(),
        }
    }
}

fn parse(content: &str, scope: McpScope) -> Result<Vec<Entry>, String> {
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return Err("mcp.json exceeds 1 MiB".into());
    }
    let value: Value = serde_json::from_str(content).map_err(|e| {
        format!(
            "Invalid mcp.json syntax at line {}, column {}",
            e.line(),
            e.column()
        )
    })?;
    let object = value.as_object().ok_or("mcp.json must be an object")?;
    if object.get("version").is_some_and(|v| v != 1) {
        return Err("Unsupported mcp.json version (expected 1)".into());
    }
    let servers = object
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or("mcp.json needs an mcpServers object")?;
    let mut entries = Vec::new();
    for (name, value) in servers {
        if name.trim().is_empty() || name != name.trim() {
            return Err(
                "MCP server names must be non-empty, without surrounding whitespace".into(),
            );
        }
        let mut config = value
            .as_object()
            .cloned()
            .ok_or_else(|| format!("MCP server {name} must be an object"))?;
        let enabled = match config.remove("enabled") {
            None => true,
            Some(Value::Bool(value)) => value,
            _ => return Err(format!("MCP server {name}: enabled must be boolean")),
        };
        // Accept the common mcpServers format without a redundant explicit type.
        if !config.contains_key("type") {
            if config.contains_key("command") {
                config.insert("type".into(), json!("stdio"));
            } else if config.contains_key("url") {
                config.insert("type".into(), json!("http"));
            }
        }
        let transport = if !enabled && !config.contains_key("type") {
            None
        } else {
            if config.get("type") == Some(&json!("sse")) {
                return Err(format!(
                    "MCP server {name}: legacy SSE is not supported; use a Streamable HTTP endpoint"
                ));
            }
            let transport: McpTransport = serde_json::from_value(Value::Object(config)).map_err(|_| format!("MCP server {name}: invalid transport fields (stdio command/args/env/cwd or http url/headers)"))?;
            // Validate static fields without resolving environment references or executing anything.
            match &transport {
                McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                    return Err(format!("MCP server {name}: command is empty"));
                }
                McpTransport::Http { url, headers } => {
                    let placeholder =
                        |value: &str| expand_with(value, |_| Ok("placeholder".into()));
                    let checked_url = placeholder(url)?;
                    let checked_url = if url.starts_with('$') {
                        "https://mcp.invalid".into()
                    } else {
                        checked_url
                    };
                    let config = McpServerConfig::http(name, checked_url).headers(
                        headers
                            .iter()
                            .map(|(k, v)| Ok((k.clone(), placeholder(v)?)))
                            .collect::<Result<_, String>>()?,
                    );
                    pi_mcp::validate_configs(&[config]).map_err(|e| e.to_string())?;
                }
                _ => {}
            }
            Some(transport)
        };
        entries.push(Entry {
            name: name.clone(),
            scope,
            transport,
            enabled,
        });
    }
    Ok(entries)
}

fn read_document(path: &Path) -> Result<Option<String>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    let mut content = String::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(io_error)?;
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return Err("mcp.json exceeds 1 MiB".into());
    }
    Ok(Some(content))
}
fn revision(content: Option<&str>) -> String {
    content.map_or_else(
        || "missing".into(),
        |content| format!("{:x}", Sha256::digest(content.as_bytes())),
    )
}
fn io_error(error: std::io::Error) -> String {
    format!("MCP configuration I/O failed: {}", error.kind())
}

fn expand_env(value: &str) -> Result<String, String> {
    expand_with(value, |key| {
        std::env::var(key)
            .map_err(|_| format!("MCP environment variable {key} is not set or is not UTF-8"))
    })
}
fn expand_with(
    value: &str,
    lookup: impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let valid = |key: &str| {
        !key.is_empty()
            && key
                .bytes()
                .enumerate()
                .all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    };
    if let Some(key) = value.strip_prefix('$').filter(|key| valid(key)) {
        return lookup(key);
    }
    let mut result = String::new();
    let mut rest = value;
    while let Some((before, after)) = rest.split_once("${") {
        result.push_str(before);
        let (key, after) = after
            .split_once('}')
            .ok_or("Unclosed MCP environment reference")?;
        let key = key.strip_prefix("env:").unwrap_or(key);
        if !valid(key) {
            return Err("Invalid MCP environment variable name".into());
        }
        result.push_str(&lookup(key)?);
        rest = after;
    }
    result.push_str(rest);
    Ok(result)
}

struct McpProductPlugin {
    library: McpLibrary,
    rows: Vec<McpServerRow>,
    pool: McpToolSet,
}
#[pi_core::agent_plugin]
impl AgentPlugin for McpProductPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("mcp")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        self.pool.plugin().register(context)?;
        context.register_command(Arc::new(McpCommand {
            library: self.library.clone(),
            rows: self.rows.clone(),
            pool: self.pool.clone(),
        }))
    }
}
struct McpCommand {
    library: McpLibrary,
    rows: Vec<McpServerRow>,
    pool: McpToolSet,
}
#[async_trait]
impl Command for McpCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "mcp".into(),
            description: "Inspect MCP servers, test connections or reload configuration".into(),
            argument_hint: Some("[status|paths|test <name>|reload]".into()),
        }
    }
    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        let arguments = arguments.trim();
        let output = match arguments {
            "reload" => {
                let replacement = context.session.reload().await?;
                replacement
                    .ui
                    .notify(NoticeLevel::Info, "MCP configuration reloaded")?;
                return Ok(CommandOutcome::Handled);
            }
            "paths" => format!(
                "Global: {}\nProject: {}\nEdit mcp.json, then /mcp reload. Project entries replace global entries by name.",
                self.library
                    .path(McpScope::Global)
                    .map_err(CommandError::Execution)?
                    .display(),
                self.library
                    .path(McpScope::Project)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| e)
            ),
            "" | "status" => {
                let tools = self.pool.tools();
                let rows = self
                    .rows
                    .iter()
                    .map(|row| {
                        format!(
                            "{} · {} · {:?} · {}",
                            row.name,
                            row.transport,
                            row.scope,
                            if row.enabled {
                                format!(
                                    "{} tools discovered",
                                    tools.iter().filter(|t| t.server_name == row.name).count()
                                )
                            } else {
                                "disabled".into()
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "MCP generation snapshot\n{}\n/mcp paths · /mcp test <name> · /mcp reload",
                    if rows.is_empty() {
                        "No configured servers"
                    } else {
                        &rows
                    }
                )
            }
            _ if arguments.starts_with("test ") => {
                let names = tokio::select! {
                    result = self.library.test(arguments[5..].trim()) => result.map_err(CommandError::Execution)?,
                    () = context.signal().wait() => return Err(CommandError::Aborted),
                };
                format!(
                    "MCP connection succeeded: {} tools\n{}",
                    names.len(),
                    names.join("\n")
                )
            }
            _ => {
                return Err(CommandError::InvalidArguments(
                    "Use /mcp [status|paths|test <name>|reload]".into(),
                ));
            }
        };
        context.ui.notify(NoticeLevel::Info, output)?;
        Ok(CommandOutcome::Handled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_replaces_whole_servers_and_can_disable_inherited_servers() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::write(agent.join("mcp.json"), r#"{"mcpServers":{"same":{"command":"global","env":{"PRIVATE":"secret"}},"off":{"command":"unused"}}}"#).unwrap();
        fs::write(project.join(".pi/mcp.json"), r#"{"version":1,"mcpServers":{"same":{"url":"https://example.com/mcp"},"off":{"enabled":false}}}"#).unwrap();
        let library = McpLibrary::new(&agent, Some(&project), true);
        let entries = library.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].enabled);
        assert!(
            matches!(&entries[1].transport, Some(McpTransport::Http { headers, .. }) if headers.is_empty())
        );
        let untrusted = McpLibrary::new(&agent, Some(&project), false);
        assert!(untrusted.read(McpScope::Project).is_err());
        assert!(untrusted.save(McpScope::Project, "missing", EMPTY).is_err());
        assert!(
            untrusted
                .entries()
                .unwrap()
                .iter()
                .all(|entry| entry.scope == McpScope::Global)
        );
        // Malformed project bytes must not affect an untrusted generation.
        fs::write(project.join(".pi/mcp.json"), "broken").unwrap();
        assert!(untrusted.entries().is_ok());
    }

    #[test]
    fn raw_edits_preserve_unknown_fields_and_detect_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let library = McpLibrary::new(dir.path(), None, false);
        let initial = library.read(McpScope::Global).unwrap();
        let content = r#"{"version":1,"future":{"kept":true},"mcpServers":{"remote":{"type":"http","url":"https://example.com/mcp","headers":{"Authorization":"Bearer ${TOKEN}"},"custom":42}}}"#;
        let saved = library
            .save(McpScope::Global, &initial.revision, content)
            .unwrap();
        assert_eq!(saved.content, content);
        assert!(
            library
                .save(McpScope::Global, &initial.revision, EMPTY)
                .err()
                .unwrap()
                .contains("changed on disk")
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("mcp.json")).unwrap(),
            content
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("mcp.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_files_remain_editable_and_diagnostics_do_not_echo_values() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("mcp.json"), "{secret").unwrap();
        let library = McpLibrary::new(dir.path(), None, false);
        let doc = library.read(McpScope::Global).unwrap();
        assert_eq!(doc.content, "{secret");
        assert!(!doc.diagnostic.as_ref().unwrap().contains("secret"));
        let error = library.save(McpScope::Global, &doc.revision, r#"{"mcpServers":{"remote":{"type":"http","url":"https://example.com","headers":{"Authorization":false}}}}"#).err().unwrap();
        assert!(error.contains("invalid transport fields"));
        library
            .save(McpScope::Global, &doc.revision, EMPTY)
            .unwrap();
        assert!(
            parse(
                r#"{"mcpServers":{"old":{"type":"sse","url":"https://example.com/sse"}}}"#,
                McpScope::Global
            )
            .is_err()
        );
    }

    #[test]
    fn environment_expansion_is_deferred_and_never_runs_shell_commands() {
        let input = r#"{"mcpServers":{"remote":{"type":"http","url":"${MCP_ENDPOINT}","headers":{"Authorization":"Bearer ${env:TOKEN}"}}}}"#;
        assert!(parse(input, McpScope::Global).is_ok());
        let lookup = |key: &str| match key {
            "TOKEN" => Ok("secret".into()),
            _ => Err("missing".into()),
        };
        assert_eq!(
            expand_with("Bearer ${TOKEN}", lookup).unwrap(),
            "Bearer secret"
        );
        assert_eq!(expand_with("$TOKEN", lookup).unwrap(), "secret");
        assert_eq!(expand_with("${env:TOKEN}", lookup).unwrap(), "secret");
        assert_eq!(
            expand_with("$(do-not-execute)", lookup).unwrap(),
            "$(do-not-execute)"
        );
        assert!(expand_with("${MISSING}", lookup).is_err());
        assert!(expand_with("${UNCLOSED", lookup).is_err());
    }

    #[tokio::test]
    async fn invalid_reload_keeps_previous_generation_and_acp_mode_skips_files() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            agent.join("memory.json"),
            r#"{"version":1,"enabled":false}"#,
        )
        .unwrap();
        let mut config = crate::ProductConfig::new(project.clone(), agent.clone());
        config.discover_extensions = false;
        config.trust_override = Some(false);
        let pi = crate::Pi::builder(config.clone()).build().unwrap();
        let session = pi
            .sessions()
            .create_session(&project, agent.join("test.jsonl"))
            .await
            .unwrap();
        assert!(
            session
                .current()
                .runtime()
                .command_specs()
                .iter()
                .any(|spec| spec.name == "mcp")
        );
        let before = session.current();
        before.submit("/mcp paths").await.unwrap();
        fs::write(agent.join("mcp.json"), "invalid").unwrap();
        assert!(session.reload().await.is_err());
        assert!(Arc::ptr_eq(&before, &session.current()));
        assert!(!agent.join("test.jsonl").exists());
        fs::write(agent.join("mcp.json"), EMPTY).unwrap();
        session.current().submit("/mcp reload").await.unwrap();
        assert!(!Arc::ptr_eq(&before, &session.current()));
        assert!(!agent.join("test.jsonl").exists());
        fs::write(agent.join("mcp.json"), "invalid").unwrap();
        config.load_mcp_config = false;
        let isolated = crate::Pi::builder(config).build().unwrap();
        let acp = isolated
            .sessions()
            .create_session(&project, agent.join("acp.jsonl"))
            .await
            .unwrap();
        assert!(
            !acp.current()
                .runtime()
                .command_specs()
                .iter()
                .any(|spec| spec.name == "mcp")
        );
        pi.sessions().shutdown().await.unwrap();
        isolated.sessions().shutdown().await.unwrap();
    }
}
