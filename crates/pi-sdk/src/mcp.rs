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
use pi_mcp::config::{MAX_CONFIG_BYTES, McpConfigContext, McpConfigEntry, parse_config};
use pi_mcp::{McpServerConfig, McpToolSet, McpTransport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ProjectTrustEvaluation, ProjectTrustService};

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
    scope: McpScope,
    config: McpConfigEntry,
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
            for config in parse_config(&content)? {
                merged.insert(config.name().to_string(), Entry { scope, config });
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
        parse_config(content)?;
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
            .find(|entry| entry.config.name() == name)
            .ok_or("MCP server not found")?;
        if !entry.config.enabled() {
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
            .filter(|entry| entry.config.enabled())
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
        let path = self.path(entry.scope)?;
        entry.config.resolve(McpConfigContext {
            config_dir: path.parent().ok_or("Invalid MCP configuration path")?,
            default_cwd: self.cwd.as_ref().unwrap_or(&self.agent_dir),
        })
    }
}

impl Entry {
    fn row(&self) -> McpServerRow {
        McpServerRow {
            name: self.config.name().to_string(),
            scope: self.scope,
            enabled: self.config.enabled(),
            transport: match self.config.transport() {
                Some(McpTransport::Http { .. }) => "http",
                Some(McpTransport::Stdio { .. }) => "stdio",
                None => "disabled",
            }
            .into(),
        }
    }
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
        assert!(!entries[0].config.enabled());
        assert!(
            matches!(entries[1].config.transport(), Some(McpTransport::Http { headers, .. }) if headers.is_empty())
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
    fn scoped_resolution_supplies_configuration_and_session_directories() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::write(
            agent.join("mcp.json"),
            r#"{"mcpServers":{"global":{"command":"server","cwd":"work"},"default":{"command":"server"}}}"#,
        )
        .unwrap();
        fs::write(
            project.join(".pi/mcp.json"),
            r#"{"mcpServers":{"project":{"command":"server","cwd":"work"}}}"#,
        )
        .unwrap();
        let library = McpLibrary::new(&agent, Some(&project), true);
        for entry in library.entries().unwrap() {
            let expected = match entry.config.name() {
                "global" => agent.join("work"),
                "project" => project.join(".pi/work"),
                "default" => project.clone(),
                name => panic!("unexpected entry {name}"),
            };
            let resolved = library.resolve(entry).unwrap();
            assert!(
                matches!(resolved.transport, McpTransport::Stdio { cwd: Some(cwd), .. } if cwd == expected)
            );
        }
        let global = McpLibrary::new(&agent, None, false);
        let entry = global
            .entries()
            .unwrap()
            .into_iter()
            .find(|entry| entry.config.name() == "default")
            .unwrap();
        let resolved = global.resolve(entry).unwrap();
        assert!(
            matches!(resolved.transport, McpTransport::Stdio { cwd: Some(cwd), .. } if cwd == agent)
        );
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
            parse_config(
                r#"{"mcpServers":{"old":{"type":"sse","url":"https://example.com/sse"}}}"#,
            )
            .is_err()
        );
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
        let mut config = crate::Config::new(project.clone(), agent.clone());
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
