//! Product-owned MCP configuration. Disk views never initialize a session or connect.
//! Executable views belong to a generation; local exposure defaults to codemode as in Pi.
mod selection;
mod supervised;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
pub use supervised::McpServerReadiness;

use crate::config::{MAX_CONFIG_BYTES, McpConfigContext, McpConfigEntry, parse_document};
use crate::policy::{McpExposure, McpServerPolicy};
use crate::{
    McpClientAuth, McpOAuthCredentialStore, McpOAuthError, McpOAuthRedirect, McpOAuthSignIn,
    McpProviderTokenResolver, McpServerConfig, McpToolSet, McpTransport,
};
use futures::future::join_all;
use pi_core::AbortSignal;
use pi_plugin::Plugin;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
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
    oauth_host: Arc<dyn crate::McpOAuthHost>,
    allow_command_values: bool,
    // Executable objects must never keep the live session's supervisor alive.
    generation: Option<crate::WeakMcpGenerationReadiness>,
    server_readiness: BTreeMap<String, McpServerReadiness>,
    server_directory: Option<String>,
    pending_direct: Vec<String>,
    preserved_tool_selection: Vec<String>,
    tool_selection_candidates: Vec<String>,
    tool_selection_reader: Option<selection::ToolSelectionReader>,
    selection_ambiguities: usize,
    denied_selection_count: usize,
}

/// A candidate product plugin plus immutable typed readiness diagnostics.
/// Publication and any automatic generation replacement remain host-owned.
pub struct PreparedMcpGeneration {
    pub plugin: Arc<dyn Plugin>,
    pub authorization_required: Vec<crate::McpAuthorizationRequired>,
    /// The host moves this sole strong ownership lease into generation freshness.
    /// Legacy strict preparation does not produce a supervisor lease.
    pub readiness_watch: Option<crate::McpGenerationReadiness>,
    pub server_readiness: BTreeMap<String, McpServerReadiness>,
    /// Exact policy-qualified selection intents absent only because a server is unavailable.
    /// These are not executable tools; the host must independently enforce its grants.
    pub temporarily_unavailable_tools: Vec<String>,
    /// Desired-only identities whose origin/visibility needs a completed inventory.
    /// A host must never interpret these as executable or newly selected tools.
    pub unresolved_tool_selection: Vec<String>,
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
    /// Scope receiving policy edits; may differ from the transport's defining scope.
    pub scope: McpScope,
    pub defining_scope: McpScope,
    pub supports_oauth: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_provider: Option<String>,
    pub transport: String,
    pub enabled: bool,
    pub exposure: McpExposure,
}

/// A typed server definition accepted by the management control plane.
#[derive(Clone, Debug)]
pub struct McpServerDefinition {
    pub name: String,
    pub transport: McpTransport,
    pub exposure: Option<McpExposure>,
    pub description: Option<String>,
    /// Unresolved settings: authoring validates them without resolving secrets.
    pub oauth: Option<crate::McpOAuthConfig>,
}

impl McpServerDefinition {
    pub fn new(name: impl Into<String>, transport: McpTransport) -> Self {
        Self {
            name: name.into(),
            transport,
            exposure: None,
            description: None,
            oauth: None,
        }
    }

    pub fn exposure(mut self, exposure: McpExposure) -> Self {
        self.exposure = Some(exposure);
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn oauth(mut self, oauth: crate::McpOAuthConfig) -> Self {
        self.oauth = Some(oauth);
        self
    }

    fn value(&self) -> Value {
        let mut object = Map::new();
        match &self.transport {
            McpTransport::Http { url, headers } => {
                object.insert("url".into(), json!(url));
                if !headers.is_empty() {
                    object.insert("headers".into(), json!(headers));
                }
            }
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                object.insert("command".into(), json!(command));
                if !args.is_empty() {
                    object.insert("args".into(), json!(args));
                }
                if !env.is_empty() {
                    object.insert("env".into(), json!(env));
                }
                if let Some(cwd) = cwd {
                    object.insert("cwd".into(), json!(cwd));
                }
            }
        }
        if let Some(exposure) = self.exposure {
            object.insert("exposure".into(), json!(exposure.as_str()));
        }
        if let Some(description) = &self.description {
            object.insert("description".into(), json!(description));
        }
        if let Some(oauth) = &self.oauth {
            object.insert("oauth".into(), json!(oauth));
        }
        Value::Object(object)
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum McpServerState {
    Pending,
    Connected,
    Disconnected,
    NeedsAuth,
    Disabled,
    Failed,
}

impl McpServerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::NeedsAuth => "needs sign-in",
            Self::Disabled => "disabled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerReport {
    pub name: String,
    pub scope: McpScope,
    pub source: PathBuf,
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    pub project_override: Option<PathBuf>,
    pub enabled: bool,
    pub exposure: McpExposure,
    pub transport: String,
    pub state: McpServerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_provider: Option<String>,
    pub tools: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_exposure: BTreeMap<String, McpExposure>,
    #[serde(skip_serializing_if = "is_false")]
    pub resources: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInspection {
    pub servers: Vec<McpServerReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl McpInspection {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
            && self.servers.iter().all(|server| {
                matches!(
                    server.state,
                    McpServerState::Connected | McpServerState::Disabled
                )
            })
    }
}

#[derive(Clone)]
struct Entry {
    /// Scope that owns the transport and its expansion provenance.
    scope: McpScope,
    /// Scope that owns the effective policy and should receive management writes.
    management_scope: McpScope,
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
            allow_command_values: false,
            generation: None,
            server_readiness: BTreeMap::new(),
            server_directory: None,
            pending_direct: Vec::new(),
            preserved_tool_selection: Vec::new(),
            tool_selection_candidates: Vec::new(),
            tool_selection_reader: None,
            selection_ambiguities: 0,
            denied_selection_count: 0,
            oauth_host: Arc::new(crate::login::ManualOAuthHost),
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

    /// Explicitly permits command-backed values for eligible local configuration at connection
    /// time. This never grants project trust and never applies to transport-only/ACP servers.
    /// Read/save/credential metadata remain non-executing. Disabled entries never run helpers.
    pub fn with_command_values(mut self, enabled: bool) -> Self {
        self.allow_command_values = enabled;
        self
    }

    pub fn with_oauth_host(mut self, host: Arc<dyn crate::McpOAuthHost>) -> Self {
        self.oauth_host = host;
        self
    }

    /// Probe before requesting interaction; a successful anonymous or already
    /// authorized connection is not sent through a new login. Reconnect after
    /// token exchange so success means the server actually accepts the result.
    pub async fn login(
        &self,
        name: &str,
        scope: Option<McpScope>,
        prompt: Arc<dyn crate::McpOAuthPrompt>,
        signal: AbortSignal,
    ) -> Result<usize, McpOAuthError> {
        let operation = async {
            let entry = self
                .management_oauth_entry(name, scope)
                .map_err(McpOAuthError::Failed)?;
            let config = self
                .resolve_connection(entry.clone(), signal.clone())
                .await
                .map_err(McpOAuthError::Failed)?;
            let challenge = match self.connect_probe(config.clone()).await {
                Ok(pool) => {
                    let count = pool.tools().len();
                    pool.shutdown()
                        .await
                        .map_err(|error| McpOAuthError::Failed(error.to_string()))?;
                    return Ok(count);
                }
                Err(crate::McpError::AuthorizationRequired(required)) => required.challenge,
                Err(error) => return Err(McpOAuthError::Failed(error.to_string())),
            };
            let McpTransport::Http { url, .. } = &config.transport else {
                unreachable!("validated HTTP")
            };
            let request = crate::McpOAuthLoginRequest {
                name: name.into(),
                server_url: url.clone(),
                settings: self
                    .local_values()
                    .oauth(config.oauth.clone().unwrap_or_default(), signal.clone())
                    .await
                    .map_err(McpOAuthError::Failed)?,
                store: self.client_auth.oauth_store.clone(),
                challenge,
            };
            self.oauth_host
                .sign_in(request, prompt, signal.clone())
                .await?;
            let config = self
                .resolve_connection(entry, signal.clone())
                .await
                .map_err(McpOAuthError::Failed)?;
            let pool = self
                .connect_probe(config)
                .await
                .map_err(|error| McpOAuthError::Failed(format!("Signed in, but {error}")))?;
            let count = pool.tools().len();
            pool.shutdown()
                .await
                .map_err(|error| McpOAuthError::Failed(error.to_string()))?;
            Ok(count)
        };
        tokio::select! {
            biased;
            () = signal.wait() => Err(McpOAuthError::Cancelled),
            result = operation => result,
        }
    }

    pub(crate) fn unknown_server_message(&self, name: &str) -> String {
        let names = self
            .inspection_entries()
            .0
            .into_iter()
            .map(|entry| entry.config.name().to_owned())
            .collect::<Vec<_>>();
        let suggestion = names
            .iter()
            .find(|candidate| name_distance(candidate, name) <= 2)
            .map(|candidate| format!(" Did you mean {candidate:?}?"))
            .unwrap_or_default();
        let ignored = self.cwd.as_ref().filter(|cwd| !self.trusted && cwd.join(".pi/mcp.json").exists())
            .map(|cwd| format!(" {} is ignored because the project is not trusted; start pi in the project to trust it.", cwd.join(".pi/mcp.json").display()))
            .unwrap_or_default();
        format!(
            "No MCP server named {name:?}.{suggestion}{ignored} Configured: {}.",
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            }
        )
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

    /// Starts real OAuth authorization for a manual-redirect frontend. No browser or listener
    /// is created here. A scope restricts the defining transport, not a project policy override.
    pub async fn begin_login(
        &self,
        name: &str,
        scope: Option<McpScope>,
        signal: AbortSignal,
    ) -> Result<(McpOAuthSignIn, McpOAuthRedirect), McpOAuthError> {
        let entry = self
            .management_oauth_entry(name, scope)
            .map_err(McpOAuthError::Failed)?;
        let config = self
            .resolve_oauth_entry(entry, signal.clone())
            .await
            .map_err(McpOAuthError::Failed)?;
        let McpTransport::Http { url, .. } = &config.transport else {
            unreachable!("validated HTTP")
        };
        let settings = config.oauth.unwrap_or_default();
        let redirect_url = settings.callback_url.clone().unwrap_or_else(|| {
            format!(
                "http://127.0.0.1:{}/callback",
                settings.callback_port.unwrap_or(53682)
            )
        });
        McpOAuthSignIn::start(
            name,
            url,
            &settings,
            self.client_auth.oauth_store.clone(),
            &redirect_url,
            None,
            signal,
        )
        .await
    }

    /// Deletes only this configured server's real OAuth credentials; provider/header auth is
    /// rejected instead of claiming to sign it out. No network request is sent.
    pub fn logout(&self, name: &str, scope: Option<McpScope>) -> Result<String, String> {
        let config = self.management_oauth_config(name, scope)?;
        let McpTransport::Http { url, .. } = &config.transport else {
            unreachable!("validated HTTP")
        };
        let removed = self.client_auth.oauth_store.sign_out(name, url)?;
        self.invalidate_active(name);
        Ok(if removed {
            format!("Signed out of MCP server {name:?}.")
        } else {
            format!("No stored credentials for MCP server {name:?}.")
        })
    }

    fn management_oauth_config(
        &self,
        name: &str,
        scope: Option<McpScope>,
    ) -> Result<McpServerConfig, String> {
        self.resolve_identity(self.management_oauth_entry(name, scope)?)
    }

    fn management_oauth_entry(&self, name: &str, scope: Option<McpScope>) -> Result<Entry, String> {
        if let Some(scope) = scope {
            self.path(scope)?;
        }
        let entry = self
            .entries()?
            .into_iter()
            .find(|entry| entry.config.name() == name)
            .ok_or_else(|| self.unknown_server_message(name))?;
        if scope.is_some_and(|scope| scope != entry.scope) {
            return Err("MCP server is defined in another scope; omit --local for an inherited global server".into());
        }
        if let Some(provider) = entry.config.auth_provider() {
            return Err(format!(
                "This MCP server uses provider credentials; use pi auth login/logout {provider}"
            ));
        }
        if !entry.config.supports_oauth() {
            return Err("MCP OAuth requires an HTTP server without an Authorization header".into());
        }
        Ok(entry)
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
                merge_entry(&mut merged, scope, config)?;
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

    /// Checks the named scope's raw definition without conflating inherited server rows.
    pub fn contains_definition(&self, scope: McpScope, name: &str) -> Result<bool, String> {
        let content = read_document(&self.path(scope)?)?.unwrap_or_else(|| "{}".into());
        let mut document = editable_document(&content)?;
        Ok(server_object_mut(&mut document)?.contains_key(name))
    }

    /// Add or replace one validated server, preserving unrelated fields and even invalid entries
    /// so management can repair a file that cannot currently prepare a generation.
    pub fn add(&self, scope: McpScope, server: &McpServerDefinition) -> Result<bool, String> {
        let value = server.value();
        let checked = parse_document(&json!({"mcpServers": {&server.name: &value}}).to_string())?;
        if checked.servers[0].is_override() {
            return Err("An added MCP server needs command or url".into());
        }
        let current = self.read(scope)?;
        let mut document = if current.revision == "missing" {
            json!({})
        } else {
            editable_document(&current.content)?
        };
        let servers = server_object_mut(&mut document)?;
        if servers.keys().any(|name| {
            name != &server.name && name.replace('-', "_") == server.name.replace('-', "_")
        }) {
            return Err(format!(
                "MCP server {}: namespace conflicts with another server",
                server.name
            ));
        }
        let replaced = servers.insert(server.name.clone(), value).is_some();
        self.replace_document(
            scope,
            &current.revision,
            &render_document(&document)?,
            false,
        )?;
        Ok(replaced)
    }

    /// Remove one server from its defining scope. Missing entries do not write a file.
    pub fn remove(&self, scope: McpScope, name: &str) -> Result<bool, String> {
        let current = self.read(scope)?;
        let mut document = editable_document(&current.content)?;
        let servers = server_object_mut(&mut document)?;
        if servers.shift_remove(name).is_none() {
            return Ok(false);
        }
        self.replace_document(
            scope,
            &current.revision,
            &render_document(&document)?,
            false,
        )?;
        self.invalidate_active(name);
        Ok(true)
    }

    /// Enable or disable a server in one scope. A project patch may override a global server.
    pub fn set_enabled(&self, scope: McpScope, name: &str, enabled: bool) -> Result<(), String> {
        self.patch_policy(scope, name, |server, is_override| {
            if enabled && !is_override {
                server.remove("enabled");
            } else {
                server.insert("enabled".into(), json!(enabled));
            }
        })?;
        if !enabled {
            self.invalidate_active(name);
        }
        Ok(())
    }

    /// Change a server's default tool exposure in one scope.
    pub fn set_exposure(
        &self,
        scope: McpScope,
        name: &str,
        exposure: McpExposure,
    ) -> Result<(), String> {
        self.patch_policy(scope, name, |server, is_override| {
            if exposure == McpExposure::Codemode && !is_override {
                server.remove("exposure");
            } else {
                server.insert("exposure".into(), json!(exposure.as_str()));
            }
        })
    }

    fn patch_policy(
        &self,
        scope: McpScope,
        name: &str,
        patch: impl FnOnce(&mut Map<String, Value>, bool),
    ) -> Result<(), String> {
        let current = self.read(scope)?;
        let mut document = editable_document(&current.content)?;
        let servers = server_object_mut(&mut document)?;
        if !servers.contains_key(name) {
            if scope != McpScope::Project
                || !self
                    .entries()?
                    .iter()
                    .any(|entry| entry.config.name() == name)
            {
                return Err(format!("No MCP server named {name:?} in this scope"));
            }
            servers.insert(name.into(), Value::Object(Map::new()));
        }
        let server = servers
            .get_mut(name)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("MCP server {name} must be an object"))?;
        let is_override = !server
            .keys()
            .any(|field| matches!(field.as_str(), "command" | "url" | "type"));
        patch(server, is_override);
        self.save(scope, &current.revision, &render_document(&document)?)?;
        Ok(())
    }

    /// Probe every configured server independently. A failed server does not hide healthy rows.
    pub async fn inspect(&self) -> McpInspection {
        let (entries, errors) = self.inspection_entries();
        let servers = join_all(entries.into_iter().map(|entry| {
            let library = self.clone();
            async move { library.inspect_entry(entry).await }
        }))
        .await;
        McpInspection { servers, errors }
    }

    fn inspection_entries(&self) -> (Vec<Entry>, Vec<String>) {
        let mut merged = BTreeMap::new();
        let mut errors = Vec::new();
        for scope in [McpScope::Global, McpScope::Project] {
            if scope == McpScope::Project && (!self.trusted || self.cwd.is_none()) {
                continue;
            }
            let loaded = (|| {
                let path = self.path(scope)?;
                let content = read_document(&path)?.unwrap_or_else(|| "{}".into());
                let mut document = editable_document(&content)?;
                let servers = server_object_mut(&mut document)?.clone();
                document
                    .as_object_mut()
                    .expect("validated document")
                    .shift_remove("mcpServers");
                if let Err(error) = parse_document(&document.to_string()) {
                    errors.push(format!("{}: {error}", path.display()));
                }
                for (name, value) in servers {
                    let parsed = parse_document(&json!({"mcpServers": {name: value}}).to_string())
                        .and_then(|mut parsed| {
                            merge_entry(&mut merged, scope, parsed.servers.remove(0))
                        });
                    if let Err(error) = parsed {
                        errors.push(format!("{}: {error}", path.display()));
                    }
                }
                Ok::<_, String>(())
            })();
            if let Err(error) = loaded {
                errors.push(error);
            }
        }
        (merged.into_values().collect(), errors)
    }

    async fn inspect_entry(&self, entry: Entry) -> McpServerReport {
        let policy = entry.config.policy().clone();
        let mut report = McpServerReport {
            name: entry.config.name().to_string(),
            scope: entry.scope,
            source: self
                .path(entry.scope)
                .expect("entry has a configured scope"),
            project_override: (entry.management_scope != entry.scope).then(|| {
                self.path(entry.management_scope)
                    .expect("entry has a configured scope")
            }),
            enabled: entry.config.enabled(),
            exposure: policy.exposure,
            transport: describe_transport(entry.config.transport()),
            state: McpServerState::Disabled,
            auth_provider: entry.config.auth_provider().map(str::to_owned),
            tools: Vec::new(),
            tool_exposure: BTreeMap::new(),
            resources: false,
            error: None,
        };
        if !report.enabled {
            return report;
        }
        report.state = McpServerState::Failed;
        let (config, mut inspection_policy) = match self.resolve_with_policy(entry).await {
            Ok(config) => config,
            Err(error) => {
                report.error = Some(error);
                return report;
            }
        };
        // Inspection inventories the remote surface, including hidden tools. It does not
        // publish a registry or change the generation's actual exposure policy.
        inspection_policy.exposure = McpExposure::Direct;
        inspection_policy.tool_exposure.clear();
        match McpToolSet::connect_with_policies_and_auth(
            vec![(config, inspection_policy)],
            self.connection_auth(),
        )
        .await
        {
            Ok(pool) => {
                report.tools = pool
                    .tools()
                    .into_iter()
                    .map(|tool| tool.remote_name)
                    .collect();
                report.tool_exposure = report
                    .tools
                    .iter()
                    .filter_map(|name| {
                        let exposure = policy.exposure_for(name);
                        (exposure != policy.exposure).then(|| (name.clone(), exposure))
                    })
                    .collect();
                report.resources = pool.has_resources(&report.name);
                report.error = pool.shutdown().await.err().map(|error| error.to_string());
                if report.error.is_none() {
                    report.state = McpServerState::Connected;
                }
            }
            Err(error) => {
                if matches!(error, crate::McpError::AuthorizationRequired(_)) {
                    report.state = McpServerState::NeedsAuth;
                }
                report.error = Some(error.to_string());
            }
        }
        report
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
        self.replace_document(scope, expected_revision, content, true)?;
        self.read(scope)
    }

    fn replace_document(
        &self,
        scope: McpScope,
        expected_revision: &str,
        content: &str,
        validate_all: bool,
    ) -> Result<(), String> {
        let path = self.path(scope)?;
        if content.len() as u64 > MAX_CONFIG_BYTES {
            return Err("mcp.json exceeds 1 MiB".into());
        }
        if validate_all {
            self.validate_save(scope, content)?;
        }
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
        if validate_all {
            self.validate_save(scope, content)?;
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        temp.write_all(content.as_bytes()).map_err(io_error)?;
        temp.as_file().sync_all().map_err(io_error)?;
        // Detect non-cooperating editors too, before the atomic replacement.
        if revision(read_document(&path)?.as_deref()) != expected_revision {
            return Err("MCP configuration changed on disk; reload before saving".into());
        }
        temp.persist(&path).map_err(|e| io_error(e.error))?;
        Ok(())
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
            .management_oauth_entry(name, None)
            .map_err(McpOAuthError::Failed)?;
        let config = self
            .resolve_oauth_entry(entry, signal.clone())
            .await
            .map_err(McpOAuthError::Failed)?;
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
        let entry = self
            .management_oauth_entry(name, None)
            .map_err(McpOAuthError::Failed)?;
        let config = self
            .resolve_oauth_entry(entry, signal.clone())
            .await
            .map_err(McpOAuthError::Failed)?;
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
        let removed = self.client_auth.oauth_store.sign_out(&config.name, &url)?;
        self.invalidate_active(name);
        Ok(removed)
    }

    fn resolve_named(&self, name: &str) -> Result<McpServerConfig, String> {
        let entry = self
            .entries()?
            .into_iter()
            .find(|entry| entry.config.name() == name)
            .ok_or("MCP server not found")?;
        self.resolve_identity(entry)
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
        let config = self.resolve_with_policy(entry).await?;
        let pool = McpToolSet::connect_with_policies_and_auth(vec![config], self.connection_auth())
            .await
            .map_err(|e| e.to_string())?;
        let names = pool.tool_names();
        pool.shutdown().await.map_err(|e| e.to_string())?;
        Ok(names)
    }

    /// Resolve only eligible credential identities. Capture their revisions before
    /// preparing connections, so a login racing preparation is observed next turn.
    pub fn credential_watch(&self) -> Result<crate::McpCredentialWatch, String> {
        let configs = self
            .entries()?
            .into_iter()
            .filter(|entry| entry.config.enabled())
            .map(|entry| self.resolve_identity(entry))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::McpCredentialWatch::new(
            self.client_auth.oauth_store.clone(),
            configs,
        ))
    }

    /// Reads eligible configuration and connects before returning an immutable plugin.
    /// The host retains candidate ownership and publishes only after complete generation validation.
    pub async fn prepare(&self) -> Result<Arc<dyn Plugin>, String> {
        Ok(self.prepare_generation().await?.plugin)
    }

    pub async fn prepare_generation(&self) -> Result<PreparedMcpGeneration, String> {
        let (entries, auto_enable_codemode) = self.configuration_replacing(None)?;
        let needs_codemode = entries
            .iter()
            .any(|entry| entry.config.enabled() && entry.config.policy().uses_codemode());
        let needs_tool_search = entries
            .iter()
            .any(|entry| entry.config.enabled() && entry.config.policy().uses_deferred());
        let rows = entries.iter().map(Entry::row).collect();
        let configs = futures::future::try_join_all(
            entries
                .into_iter()
                .filter(|entry| entry.config.enabled())
                .map(|entry| self.resolve_with_policy(entry)),
        )
        .await?;
        let pool = McpToolSet::connect_generation(configs, self.connection_auth(), true)
            .await
            .map_err(|e| e.to_string())?;
        let authorization_required = pool.authorization_required().to_vec();
        let mut library = self.clone();
        library.generation = None;
        library.server_readiness.clear();
        library.server_directory = None;
        library.pending_direct.clear();
        library.preserved_tool_selection.clear();
        library.tool_selection_candidates.clear();
        library.tool_selection_reader = None;
        library.selection_ambiguities = 0;
        library.denied_selection_count = 0;
        Ok(PreparedMcpGeneration {
            temporarily_unavailable_tools: Vec::new(),
            unresolved_tool_selection: Vec::new(),
            authorization_required,
            readiness_watch: None,
            server_readiness: BTreeMap::new(),
            plugin: Arc::new(McpProductPlugin {
                library,
                rows,
                pool,
                auto_enable_codemode,
                needs_codemode,
                needs_tool_search,
            }),
        })
    }

    async fn resolve_with_policy(
        &self,
        entry: Entry,
    ) -> Result<(McpServerConfig, McpServerPolicy), String> {
        let mut policy = entry.config.policy().clone();
        policy.root = Some(self.cwd.as_ref().unwrap_or(&self.agent_dir).clone());
        policy.log_path = Some(self.agent_dir.join("mcp.log"));
        Ok((
            self.resolve_connection(entry, pi_core::AbortHandle::new().1)
                .await?,
            policy,
        ))
    }

    fn resolve_identity(&self, entry: Entry) -> Result<McpServerConfig, String> {
        self.path(entry.scope)?;
        entry.config.resolve_identity()
    }

    fn local_values(&self) -> crate::config::LocalValues {
        crate::config::LocalValues {
            cwd: self.cwd.as_ref().unwrap_or(&self.agent_dir).clone(),
            allow_commands: self.allow_command_values,
        }
    }

    async fn connect_probe(&self, config: McpServerConfig) -> Result<McpToolSet, crate::McpError> {
        let policy = McpServerPolicy {
            log_path: Some(self.agent_dir.join("mcp.log")),
            ..McpServerPolicy::direct()
        };
        McpToolSet::connect_with_policies_and_auth(vec![(config, policy)], self.connection_auth())
            .await
    }

    fn connection_auth(&self) -> McpClientAuth {
        let mut auth = self.client_auth.clone();
        auth.local_values = Some(self.local_values());
        auth
    }

    async fn resolve_oauth_entry(
        &self,
        entry: Entry,
        signal: AbortSignal,
    ) -> Result<McpServerConfig, String> {
        if !entry.config.enabled() {
            return Err("Enable the MCP server before connecting or signing in".into());
        }
        let mut config = self.resolve_identity(entry)?;
        if let Some(settings) = config.oauth.take() {
            config.oauth = Some(self.local_values().oauth(settings, signal).await?);
        }
        Ok(config)
    }

    async fn resolve_connection(
        &self,
        entry: Entry,
        signal: AbortSignal,
    ) -> Result<McpServerConfig, String> {
        let path = self.path(entry.scope)?;
        entry
            .config
            .resolve_for_connect(
                McpConfigContext {
                    config_dir: path.parent().ok_or("Invalid MCP configuration path")?,
                    default_cwd: self.cwd.as_ref().unwrap_or(&self.agent_dir),
                },
                self.allow_command_values,
                signal,
            )
            .await
    }
}

impl Entry {
    fn row(&self) -> McpServerRow {
        McpServerRow {
            name: self.config.name().to_string(),
            scope: self.management_scope,
            defining_scope: self.scope,
            supports_oauth: self.config.supports_oauth(),
            auth_provider: self.config.auth_provider().map(str::to_owned),
            enabled: self.config.enabled(),
            exposure: self.config.policy().exposure,
            transport: match self.config.transport() {
                Some(McpTransport::Http { .. }) => "http",
                Some(McpTransport::Stdio { .. }) => "stdio",
                None => "disabled",
            }
            .into(),
        }
    }
}

fn name_distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (i, a) in left.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, b) in right.iter().enumerate() {
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + usize::from(a != *b)),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

fn merge_entry(
    merged: &mut BTreeMap<String, Entry>,
    scope: McpScope,
    config: McpConfigEntry,
) -> Result<(), String> {
    let name = config.name().to_string();
    if scope == McpScope::Project && config.has_provider_auth() {
        return Err(format!(
            "MCP server {name}: auth is only allowed in global MCP configuration"
        ));
    }
    let entry = if scope == McpScope::Project && config.is_override() {
        let base = merged.get(&name).ok_or_else(|| {
            format!("MCP server {name}: a project policy override needs a global server")
        })?;
        Entry {
            scope: base.scope,
            management_scope: McpScope::Project,
            config: config.apply_override(&base.config)?,
        }
    } else {
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
        Entry {
            scope,
            management_scope: scope,
            config,
        }
    };
    merged.insert(name, entry);
    Ok(())
}

fn editable_document(content: &str) -> Result<Value, String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let value: Value = serde_json::from_str(content).map_err(|error| {
        format!(
            "Invalid mcp.json syntax at line {}, column {}",
            error.line(),
            error.column()
        )
    })?;
    if !value.is_object() {
        return Err("mcp.json must be an object".into());
    }
    Ok(value)
}

fn server_object_mut(document: &mut Value) -> Result<&mut Map<String, Value>, String> {
    let root = document
        .as_object_mut()
        .ok_or("mcp.json must be an object")?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    servers
        .as_object_mut()
        .ok_or_else(|| "mcp.json needs an mcpServers object".into())
}

fn render_document(document: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(document)
        .map(|content| format!("{content}\n"))
        .map_err(|error| format!("Could not serialize mcp.json: {error}"))
}

fn describe_transport(transport: Option<&McpTransport>) -> String {
    match transport {
        Some(McpTransport::Http { url, .. }) => url.clone(),
        Some(McpTransport::Stdio { command, args, .. }) => std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        None => "disabled".into(),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
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

    #[tokio::test]
    async fn project_policy_overrides_keep_global_transport_and_replace_only_selected_policy() {
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
        let (_, policy) = library.resolve_with_policy(entry).await.unwrap();
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

    #[tokio::test]
    async fn scoped_resolution_supplies_configuration_and_session_directories() {
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
            let resolved = library
                .resolve_connection(entry, pi_core::AbortHandle::new().1)
                .await
                .unwrap();
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
        let resolved = global
            .resolve_connection(entry, pi_core::AbortHandle::new().1)
            .await
            .unwrap();
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

    #[test]
    fn typed_mutations_keep_unrelated_content_and_support_project_policy_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        let project = dir.path().join("project");
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::create_dir_all(&agent).unwrap();
        fs::write(
            agent.join("mcp.json"),
            r#"{"future":{"kept":true},"mcpServers":{"old":{"command":"old"}}}"#,
        )
        .unwrap();
        let library = McpLibrary::new(&agent, Some(&project), true);
        let server = McpServerDefinition::new(
            "docs",
            McpTransport::Http {
                url: "https://example.com/mcp".into(),
                headers: BTreeMap::from([("X-Team".into(), "core".into())]),
            },
        )
        .exposure(McpExposure::Direct)
        .description("Product docs");
        assert!(!library.add(McpScope::Global, &server).unwrap());
        let value: Value =
            serde_json::from_str(&fs::read_to_string(agent.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(value["future"]["kept"], true);
        assert_eq!(value["mcpServers"]["old"]["command"], "old");
        assert_eq!(value["mcpServers"]["docs"]["exposure"], "direct");
        assert_eq!(value["mcpServers"]["docs"]["headers"]["X-Team"], "core");

        library
            .set_enabled(McpScope::Project, "docs", false)
            .unwrap();
        library
            .set_exposure(McpScope::Project, "docs", McpExposure::Hidden)
            .unwrap();
        let project_value: Value =
            serde_json::from_str(&fs::read_to_string(project.join(".pi/mcp.json")).unwrap())
                .unwrap();
        assert_eq!(project_value["mcpServers"]["docs"]["enabled"], false);
        assert_eq!(project_value["mcpServers"]["docs"]["exposure"], "hidden");
        assert!(project_value["mcpServers"]["docs"].get("url").is_none());
        assert!(library.remove(McpScope::Global, "old").unwrap());
        assert!(!library.remove(McpScope::Global, "missing").unwrap());
    }

    #[tokio::test]
    async fn inspection_keeps_valid_entries_and_reports_invalid_configuration() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("mcp.json"),
            r#"{"mcpServers":{"parked":{"command":"unused","enabled":false},"bad":{"args":["no command"]}}}"#,
        )
        .unwrap();
        let library = McpLibrary::new(dir.path(), None, false);
        let inspection = library.inspect().await;
        assert!(!inspection.is_ok());
        assert_eq!(inspection.errors.len(), 1);
        assert_eq!(inspection.servers.len(), 1);
        assert_eq!(inspection.servers[0].name, "parked");
        assert!(
            library.prepare().await.is_err(),
            "generation parsing stays strict"
        );
        assert!(library.remove(McpScope::Global, "parked").unwrap());
        let remaining: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            remaining["mcpServers"]["bad"]["args"],
            json!(["no command"])
        );
    }

    #[test]
    fn management_oauth_authoring_validates_without_resolving_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let library = McpLibrary::new(dir.path(), None, false);
        let oauth = crate::McpOAuthConfig {
            client_id: Some("fixture".into()),
            client_secret: Some("${MCP_SECRET_NOT_READ_WHILE_AUTHORING}".into()),
            ..Default::default()
        };
        let definition = McpServerDefinition::new(
            "account",
            McpTransport::Http {
                url: "https://example.invalid/mcp".into(),
                headers: BTreeMap::new(),
            },
        )
        .oauth(oauth.clone());
        assert!(!format!("{definition:?}").contains("MCP_SECRET_NOT_READ_WHILE_AUTHORING"));
        assert!(!library.add(McpScope::Global, &definition).unwrap());
        let path = dir.path().join("mcp.json");
        let before = fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(
            value["mcpServers"]["account"]["oauth"]["clientSecret"],
            "${MCP_SECRET_NOT_READ_WHILE_AUTHORING}"
        );
        let invalid = McpServerDefinition::new(
            "account",
            McpTransport::Stdio {
                command: "never-started".into(),
                args: vec![],
                env: BTreeMap::new(),
                cwd: None,
            },
        )
        .oauth(oauth);
        assert!(
            library
                .add(McpScope::Global, &invalid)
                .unwrap_err()
                .contains("only valid for HTTP")
        );
        assert_eq!(fs::read(path).unwrap(), before);
        assert!(!dir.path().join("mcp-auth.json").exists());
    }

    #[test]
    fn management_preserves_tool_pattern_order_and_rejects_invalid_add_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let original = r#"{"mcpServers":{"docs":{"command":"unused","toolExposure":{"z*":"hidden","*":"direct"}}}}"#;
        fs::write(&path, original).unwrap();
        let library = McpLibrary::new(dir.path(), None, false);
        let bad = McpServerDefinition::new(
            "bad name",
            McpServerConfig::stdio("ignored", "cmd").transport,
        );
        assert!(library.add(McpScope::Global, &bad).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        library
            .set_enabled(McpScope::Global, "docs", false)
            .unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.find("z*").unwrap() < content.find("\"*\"").unwrap());
        assert_eq!(
            library.entries().unwrap()[0]
                .config
                .policy()
                .exposure_for("z-secret"),
            McpExposure::Hidden
        );
    }

    #[tokio::test]
    async fn inspection_keeps_disabled_rows_when_another_server_fails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("mcp.json"),
            r#"{"mcpServers":{"broken":{"command":"pi-test-missing-mcp-server"},"parked":{"command":"unused","enabled":false}}}"#,
        )
        .unwrap();
        let inspection = McpLibrary::new(dir.path(), None, false).inspect().await;
        assert!(!inspection.is_ok());
        assert!(inspection.errors.is_empty());
        assert_eq!(inspection.servers.len(), 2);
        assert_eq!(inspection.servers[0].name, "broken");
        assert_eq!(inspection.servers[0].state, McpServerState::Failed);
        assert_eq!(inspection.servers[1].name, "parked");
        assert_eq!(inspection.servers[1].state, McpServerState::Disabled);
    }
}
