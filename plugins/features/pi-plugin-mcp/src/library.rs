//! Product-owned MCP configuration. Disk views never initialize a session or connect.
//! Connections belong to a generation; local exposure defaults to codemode as in Pi.
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::{MAX_CONFIG_BYTES, McpConfigContext, McpConfigEntry, parse_document};
use crate::policy::McpServerPolicy;
use crate::{
    McpClientAuth, McpOAuthCredentialStore, McpOAuthError, McpOAuthRedirect, McpOAuthSignIn,
    McpProviderTokenResolver, McpServerConfig, McpToolSet, McpTransport,
};
use pi_core::AbortSignal;
use pi_plugin::Plugin;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::plugin::McpProductPlugin;

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
    pub(crate) register_tool_search: bool,
    client_auth: McpClientAuth,
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
    /// Opens a scoped disk view using the host's already-resolved project trust decision.
    /// Construction does not read configuration, connect servers or create files.
    pub fn new(agent_dir: &Path, cwd: Option<&Path>, trusted: bool) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            cwd: cwd.map(Path::to_path_buf),
            trusted,
            register_tool_search: true,
            client_auth: McpClientAuth::default().with_oauth_store(McpOAuthCredentialStore::new(
                agent_dir.join("mcp-auth.json"),
            )),
        }
    }

    /// Installs host-owned auth dependencies without coupling MCP to a product domain.
    pub fn with_client_auth(mut self, client_auth: McpClientAuth) -> Self {
        self.client_auth = client_auth;
        self
    }

    /// Adds request-time provider token lookup while retaining this library's
    /// durable OAuth store.
    pub fn with_provider_token_resolver(
        mut self,
        resolver: Arc<dyn McpProviderTokenResolver>,
    ) -> Self {
        self.client_auth = self.client_auth.with_provider_tokens(resolver);
        self
    }

    /// Controls the standalone deferred-discovery helper. Defaults to enabled.
    /// Hosts that register their own `ToolSearchPlugin` disable this to keep one owner per tool.
    /// MCP session-start activation of an available search entry point is unchanged.
    pub fn with_tool_search_registration(mut self, enabled: bool) -> Self {
        self.register_tool_search = enabled;
        self
    }

    pub(crate) fn path(&self, scope: McpScope) -> Result<PathBuf, String> {
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
        self.configuration_replacing(None)
            .map(|(entries, _)| entries)
    }

    /// Validate a proposed scope against the same inherited configuration used at startup.
    /// No project file is read until the host has granted project trust.
    fn configuration_replacing(
        &self,
        replacement: Option<(McpScope, &str)>,
    ) -> Result<(Vec<Entry>, bool), String> {
        let mut auto_enable_codemode = true;
        let mut merged: BTreeMap<String, Entry> = BTreeMap::new();
        for scope in [McpScope::Global, McpScope::Project] {
            if scope == McpScope::Project && (!self.trusted || self.cwd.is_none()) {
                continue;
            }
            let path = self.path(scope)?;
            let content = match replacement.filter(|(replaced_scope, _)| *replaced_scope == scope) {
                Some((_, content)) => content.to_owned(),
                None => read_document(&path)?.unwrap_or_else(|| EMPTY.into()),
            };
            let document = parse_document(&content)?;
            if let Some(value) = document.auto_enable_codemode {
                auto_enable_codemode = value;
            }
            for config in document.servers {
                let name = config.name().to_string();
                if scope == McpScope::Project && config.has_provider_auth() {
                    return Err(format!(
                        "MCP server {name}: auth is only allowed in global MCP configuration"
                    ));
                }
                if scope == McpScope::Project && config.is_override() {
                    let base = merged.get(&name).ok_or_else(|| {
                        format!(
                            "MCP server {name}: a project policy override needs a global server"
                        )
                    })?;
                    let entry = Entry {
                        scope: base.scope,
                        config: config.apply_override(&base.config)?,
                    };
                    merged.insert(name, entry);
                    continue;
                }
                // Keep existing standalone disabled placeholders as a Rust compatibility extension.
                if config.is_override() && config.enabled() {
                    return Err(format!(
                        "MCP server {name}: enabled server needs command or url"
                    ));
                }
                let namespace = name.replace('-', "_");
                if let Some(previous) = merged
                    .keys()
                    .find(|previous| **previous != name && previous.replace('-', "_") == namespace)
                {
                    return Err(format!(
                        "MCP server {name}: namespace conflicts with {previous}"
                    ));
                }
                merged.insert(name, Entry { scope, config });
            }
        }
        Ok((merged.into_values().collect(), auto_enable_codemode))
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

    fn validate_save(&self, scope: McpScope, content: &str) -> Result<(), String> {
        let global = Self::new(&self.agent_dir, None, false);
        if scope == McpScope::Global {
            // A broken project must not prevent repairing global configuration.
            return global
                .configuration_replacing(Some((scope, content)))
                .map(|_| ());
        }
        let candidate = parse_document(content)?.servers;
        let needs_global = candidate.iter().any(McpConfigEntry::is_override);
        if !needs_global && global.entries().is_err() {
            // Independent project definitions (or an empty file) can be repaired even
            // while the global file is broken. Preparation still validates both scopes.
            return Ok(());
        }
        self.configuration_replacing(Some((scope, content)))
            .map(|_| ())
    }

    pub fn save(
        &self,
        scope: McpScope,
        expected_revision: &str,
        content: &str,
    ) -> Result<McpDocument, String> {
        let path = self.path(scope)?;
        self.validate_save(scope, content)?;
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
        // Re-check inherited definitions after acquiring the target scope's save lock.
        self.validate_save(scope, content)?;
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

    /// Starts an explicit OAuth flow for a configured HTTP server. Connecting
    /// never invokes this method; the host decides how to open the browser and
    /// whether to receive a loopback or pasted redirect.
    pub async fn start_oauth_sign_in(
        &self,
        name: &str,
        redirect_url: &str,
        challenge: Option<&str>,
        signal: AbortSignal,
    ) -> Result<(McpOAuthSignIn, McpOAuthRedirect), McpOAuthError> {
        let entry = self
            .entries()
            .map_err(McpOAuthError::Failed)?
            .into_iter()
            .find(|entry| entry.config.name() == name)
            .ok_or_else(|| McpOAuthError::Failed("MCP server not found".into()))?;
        let config = self.resolve(entry).map_err(McpOAuthError::Failed)?;
        if config.auth.is_some() {
            return Err(McpOAuthError::Failed(
                "provider-authenticated MCP servers do not use OAuth sign-in".into(),
            ));
        }
        let McpTransport::Http { url, .. } = &config.transport else {
            return Err(McpOAuthError::Failed(
                "OAuth sign-in requires an HTTP MCP server".into(),
            ));
        };
        let settings = config.oauth.clone().unwrap_or_default();
        let redirect_url = settings.callback_url.clone().unwrap_or_else(|| {
            settings.callback_port.map_or_else(
                || redirect_url.to_string(),
                |port| format!("http://127.0.0.1:{port}/callback"),
            )
        });
        McpOAuthSignIn::start(
            &config.name,
            url,
            &settings,
            self.client_auth.oauth_store.clone(),
            &redirect_url,
            challenge,
            signal,
        )
        .await
    }

    pub async fn refresh_oauth(
        &self,
        name: &str,
        signal: AbortSignal,
    ) -> Result<(), McpOAuthError> {
        let config = self.resolve_named(name).map_err(McpOAuthError::Failed)?;
        let McpTransport::Http { url, .. } = &config.transport else {
            return Err(McpOAuthError::Failed(
                "OAuth refresh requires an HTTP MCP server".into(),
            ));
        };
        McpOAuthSignIn::refresh(
            &config.name,
            url,
            &config.oauth.unwrap_or_default(),
            self.client_auth.oauth_store.clone(),
            signal,
        )
        .await
    }

    pub fn has_oauth_credentials(&self, name: &str) -> Result<bool, String> {
        let config = self.resolve_named(name)?;
        let McpTransport::Http { url, .. } = config.transport else {
            return Ok(false);
        };
        self.client_auth
            .oauth_store
            .has_credentials(&config.name, &url)
    }

    pub fn sign_out_oauth(&self, name: &str) -> Result<bool, String> {
        let config = self.resolve_named(name)?;
        let McpTransport::Http { url, .. } = config.transport else {
            return Ok(false);
        };
        self.client_auth.oauth_store.sign_out(&config.name, &url)
    }

    fn resolve_named(&self, name: &str) -> Result<McpServerConfig, String> {
        let entry = self
            .entries()?
            .into_iter()
            .find(|entry| entry.config.name() == name)
            .ok_or("MCP server not found")?;
        self.resolve(entry)
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
        let config = self.resolve_with_policy(entry)?;
        let pool =
            McpToolSet::connect_with_policies_and_auth(vec![config], self.client_auth.clone())
                .await
                .map_err(|e| e.to_string())?;
        let names = pool.tool_names();
        pool.shutdown().await.map_err(|e| e.to_string())?;
        Ok(names)
    }

    /// Reads eligible configuration and connects before returning an immutable plugin.
    /// The host retains candidate ownership and publishes only after complete generation validation.
    pub async fn prepare(&self) -> Result<Arc<dyn Plugin>, String> {
        let (entries, auto_enable_codemode) = self.configuration_replacing(None)?;
        let needs_codemode = entries
            .iter()
            .any(|entry| entry.config.enabled() && entry.config.policy().uses_codemode());
        let needs_tool_search = entries
            .iter()
            .any(|entry| entry.config.enabled() && entry.config.policy().uses_deferred());
        let rows = entries.iter().map(Entry::row).collect();
        let configs = entries
            .into_iter()
            .filter(|entry| entry.config.enabled())
            .map(|entry| self.resolve_with_policy(entry))
            .collect::<Result<Vec<_>, _>>()?;
        let pool = McpToolSet::connect_with_policies_and_auth(configs, self.client_auth.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(McpProductPlugin {
            library: self.clone(),
            rows,
            pool,
            auto_enable_codemode,
            needs_codemode,
            needs_tool_search,
        }))
    }

    fn resolve_with_policy(
        &self,
        entry: Entry,
    ) -> Result<(McpServerConfig, McpServerPolicy), String> {
        let mut policy = entry.config.policy().clone();
        policy.root = Some(self.cwd.as_ref().unwrap_or(&self.agent_dir).clone());
        Ok((self.resolve(entry)?, policy))
    }

    fn resolve(&self, entry: Entry) -> Result<McpServerConfig, String> {
        let path = self.path(entry.scope)?;
        entry.config.resolve_transport(McpConfigContext {
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
    fn auto_enable_codemode_merges_explicit_trusted_values_and_inherits_omissions() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        for global in [None, Some(false), Some(true)] {
            for local in [None, Some(false), Some(true)] {
                let content = |flag: Option<bool>| {
                    flag.map_or_else(
                        || "{}".into(),
                        |value| serde_json::json!({"autoEnableCodemode":value}).to_string(),
                    )
                };
                fs::write(agent.join("mcp.json"), content(global)).unwrap();
                fs::write(project.join(".pi/mcp.json"), content(local)).unwrap();
                for trusted in [false, true] {
                    let library = McpLibrary::new(&agent, Some(&project), trusted);
                    let (_, enabled) = library.configuration_replacing(None).unwrap();
                    assert_eq!(
                        enabled,
                        if trusted { local.or(global) } else { global }.unwrap_or(true)
                    );
                }
            }
        }
        fs::write(
            project.join(".pi/mcp.json"),
            r#"{"autoEnableCodemode":"invalid"}"#,
        )
        .unwrap();
        assert!(
            McpLibrary::new(&agent, Some(&project), false)
                .entries()
                .is_ok()
        );
        assert!(
            McpLibrary::new(&agent, Some(&project), true)
                .entries()
                .is_err()
        );
    }

    #[test]
    fn saving_auto_enable_codemode_preserves_policy_and_rejects_invalid_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let library = McpLibrary::new(dir.path(), None, false);
        let current = library.read(McpScope::Global).unwrap();
        let content = r#"{"autoEnableCodemode":false,"mcpServers":{"docs":{"command":"server","enabled":false}}}"#;
        let saved = library
            .save(McpScope::Global, &current.revision, content)
            .unwrap();
        assert_eq!(saved.content, content);
        assert!(saved.diagnostic.is_none());
        assert_eq!(saved.servers.len(), 1);
        assert!(!library.configuration_replacing(None).unwrap().1);
        let error = library
            .save(
                McpScope::Global,
                &saved.revision,
                r#"{"autoEnableCodemode":"invalid"}"#,
            )
            .err()
            .unwrap();
        assert!(error.contains("must be a boolean"));
        assert_eq!(library.read(McpScope::Global).unwrap().content, content);
    }

    #[test]
    fn project_policy_overrides_keep_global_transport_and_replace_only_selected_policy() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::write(agent.join("mcp.json"), r#"{"mcpServers":{"server":{"command":"global","env":{"PRIVATE":"secret"},"enabled":false,"description":"Global tools","timeout":15,"exposure":"hidden","toolExposure":{"read*":"direct"}}}}"#).unwrap();
        let library = McpLibrary::new(&agent, Some(&project), true);
        let initial = library.read(McpScope::Project).unwrap();
        library.save(McpScope::Project, &initial.revision, r#"{"mcpServers":{"server":{"enabled":true,"exposure":"direct","toolExposure":{"write*":"hidden"}}}}"#).unwrap();
        let entry = library.entries().unwrap().pop().unwrap();
        assert_eq!(entry.scope, McpScope::Global);
        assert!(entry.config.enabled());
        assert!(
            matches!(entry.config.transport(), Some(McpTransport::Stdio { command, env, .. }) if command == "global" && env["PRIVATE"] == "secret")
        );
        assert_eq!(
            entry.config.policy().description.as_deref(),
            Some("Global tools")
        );
        assert_eq!(
            entry.config.policy().timeout,
            std::time::Duration::from_secs(15)
        );
        assert_eq!(
            entry.config.policy().exposure_for("write-file"),
            crate::policy::McpExposure::Hidden
        );
        assert_eq!(
            entry.config.policy().exposure_for("read-file"),
            crate::policy::McpExposure::Direct
        );
        let (_, policy) = library.resolve_with_policy(entry).unwrap();
        assert_eq!(policy.root.as_deref(), Some(project.as_path()));

        let current = library.read(McpScope::Project).unwrap();
        library
            .save(
                McpScope::Project,
                &current.revision,
                r#"{"mcpServers":{"server":{"toolExposure":{}}}}"#,
            )
            .unwrap();
        let entry = library.entries().unwrap().pop().unwrap();
        assert!(!entry.config.enabled());
        assert!(entry.config.policy().tool_exposure.is_empty());
        assert_eq!(
            entry.config.policy().exposure_for("read-file"),
            crate::policy::McpExposure::Hidden
        );
    }

    #[test]
    fn invalid_scope_overrides_are_rejected_before_saving() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::write(
            agent.join("mcp.json"),
            r#"{"mcpServers":{"global-server":{"command":"server"}}}"#,
        )
        .unwrap();
        let library = McpLibrary::new(&agent, Some(&project), true);
        for candidate in [
            r#"{"mcpServers":{"missing":{"enabled":false}}}"#,
            r#"{"mcpServers":{"global-server":{"enabled":true,"env":{"TOKEN":"secret"}}}}"#,
            r#"{"mcpServers":{"global_server":{"command":"server"}}}"#,
        ] {
            let error = library
                .save(McpScope::Project, "missing", candidate)
                .err()
                .unwrap();
            assert!(!error.contains("secret"));
            assert!(!project.join(".pi/mcp.json").exists());
        }
        library
            .save(
                McpScope::Project,
                "missing",
                r#"{"mcpServers":{"global-server":{"enabled":false}}}"#,
            )
            .unwrap();
        let global = library.read(McpScope::Global).unwrap();
        // Scoped editing can remove a global definition. The remaining project
        // override becomes a preparation diagnostic instead of blocking that edit.
        library
            .save(McpScope::Global, &global.revision, EMPTY)
            .unwrap();
        assert!(library.entries().is_err());
    }

    #[test]
    fn broken_scopes_can_be_repaired_independently_without_losing_raw_fields() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        for first in [McpScope::Global, McpScope::Project] {
            fs::write(agent.join("mcp.json"), "broken global").unwrap();
            fs::write(project.join(".pi/mcp.json"), "broken project").unwrap();
            let library = McpLibrary::new(&agent, Some(&project), true);
            let second = if first == McpScope::Global {
                McpScope::Project
            } else {
                McpScope::Global
            };
            for scope in [first, second] {
                let doc = library.read(scope).unwrap();
                library.save(scope, &doc.revision, "{}").unwrap();
            }
            assert!(library.entries().unwrap().is_empty());
        }
        let library = McpLibrary::new(&agent, Some(&project), true);
        let doc = library.read(McpScope::Global).unwrap();
        let raw = r#"{"mcpServers":{"off":{"enabled":false,"future":{"keep":true}}}}"#;
        assert_eq!(
            library
                .save(McpScope::Global, &doc.revision, raw)
                .unwrap()
                .content,
            raw
        );
        assert!(!library.entries().unwrap()[0].config.enabled());
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
                "global" | "project" => project.join("work"),
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
            crate::config::parse_config(
                r#"{"mcpServers":{"old":{"type":"sse","url":"https://example.com/sse"}}}"#,
            )
            .is_err()
        );
    }
}
