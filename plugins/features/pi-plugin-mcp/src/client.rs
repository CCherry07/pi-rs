//! Protocol-neutral MCP client integration for Pi runtime generations.
//!
//! This module owns MCP transports, discovery, invocation, and cleanup. It has
//! no knowledge of ACP or Pi session persistence; callers adapt their wire
//! configuration into [`McpServerConfig`] and inject [`McpToolSet::plugin`]
//! through their own generation seam.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    ContentBlock, PluginId, TextContent, ToolCallId, ToolExecutionMode, ToolResult, ToolSpec,
    ToolUpdate,
};
use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, Plugin, PluginError,
    RegisterContext, Tool, ToolContext, ToolDiscovery, ToolError, ToolExposure, ToolNamespace,
    ToolUpdateSink,
};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientRequest, ServerResult, Tool as McpToolSpec,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{Peer, RoleClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::auth::{McpClientAuth, ProviderAuthClient, oauth_http_client};
use crate::client_handler::McpClientHandler;
use crate::directory::render_servers_section;
use crate::naming::{identifier_component, qualified_tool_name};
use crate::policy::{McpExposure, McpServerPolicy};
use crate::resources::create_resource_tools;
use crate::result::{normalize_input_schema, project_result_with_resources};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpProviderAuth>,
}

/// OAuth client settings for a Streamable HTTP server.
///
/// Secret values are resolved by the configuration owner immediately before a
/// connection or sign-in attempt. Debug output deliberately omits their values.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_registration: Option<McpOAuthClientRegistration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_server_metadata_url: Option<String>,
}

impl std::fmt::Debug for McpOAuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpOAuthConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("callback_port", &self.callback_port)
            .field("callback_url", &self.callback_url)
            .field("scope", &self.scope)
            .field("client_name", &self.client_name)
            .field("client_registration", &self.client_registration)
            .field("auth_server_metadata_url", &self.auth_server_metadata_url)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpOAuthClientRegistration {
    #[default]
    Auto,
    Dcr,
    Cimd,
}

/// Use the current token of a registered Pi provider for every HTTP request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpProviderAuth {
    pub provider: String,
}

impl McpServerConfig {
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransport::Http {
                url: url.into(),
                headers: BTreeMap::new(),
            },
            oauth: None,
            auth: None,
        }
    }

    pub fn headers(mut self, headers: BTreeMap<String, String>) -> Self {
        if let McpTransport::Http {
            headers: configured,
            ..
        } = &mut self.transport
        {
            *configured = headers;
        }
        self
    }

    pub fn oauth(mut self, oauth: McpOAuthConfig) -> Self {
        self.oauth = Some(oauth);
        self
    }

    pub fn provider_auth(mut self, provider: impl Into<String>) -> Self {
        self.auth = Some(McpProviderAuth {
            provider: provider.into(),
        });
        self
    }

    pub fn stdio(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransport::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            },
            oauth: None,
            auth: None,
        }
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        if let McpTransport::Stdio {
            args: configured, ..
        } = &mut self.transport
        {
            *configured = args.into_iter().map(Into::into).collect();
        }
        self
    }

    pub fn env(mut self, env: BTreeMap<String, String>) -> Self {
        if let McpTransport::Stdio {
            env: configured, ..
        } = &mut self.transport
        {
            *configured = env;
        }
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        if let McpTransport::Stdio {
            cwd: configured, ..
        } = &mut self.transport
        {
            *configured = Some(cwd.into());
        }
        self
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
}

// Transport configuration can contain credentials, including arguments and URLs.
impl std::fmt::Debug for McpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stdio { .. } => "Stdio { .. }",
            Self::Http { .. } => "Http { .. }",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolDescriptor {
    pub server_name: String,
    pub remote_name: String,
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP server name must not be empty")]
    EmptyServerName,
    #[error("duplicate MCP server name: {0}")]
    DuplicateServerName(String),
    #[error("invalid MCP server name: {0} (use letters, digits, '_' and '-')")]
    InvalidServerName(String),
    #[error("MCP servers share the same namespace: {0}")]
    DuplicateNamespace(String),
    #[error("MCP stdio command for server {0} must not be empty")]
    EmptyCommand(String),
    #[error("invalid MCP HTTP configuration for server {server}: {message}")]
    InvalidHttp {
        server: String,
        message: &'static str,
    },
    #[error("MCP connection or tool discovery timed out for server {0}")]
    Timeout(String),
    #[error("failed to start MCP server {server}: {message}")]
    Start { server: String, message: String },
    #[error(transparent)]
    AuthorizationRequired(McpAuthorizationRequired),
    #[error("failed to initialize MCP server {server}: {message}")]
    Initialize { server: String, message: String },
    #[error("failed to list tools from MCP server {server}: {message}")]
    ListTools { server: String, message: String },
    #[error("duplicate qualified MCP tool name: {0}")]
    DuplicateTool(String),
    #[error("failed to shut down MCP server {server}: {message}")]
    Shutdown { server: String, message: String },
}

/// Actionable authentication state. The server's challenge is deliberately
/// excluded from Debug/Display because it may contain endpoint or account data.
#[derive(Clone)]
pub struct McpAuthorizationRequired {
    pub server: String,
    pub provider: Option<String>,
    pub static_authorization: bool,
    pub(crate) challenge: Option<String>,
}

impl McpAuthorizationRequired {
    pub fn guidance(&self) -> String {
        if self.static_authorization {
            "update the configured Authorization header".into()
        } else if let Some(provider) = &self.provider {
            format!("use pi auth login {provider}")
        } else {
            format!("use pi mcp login {}", self.server)
        }
    }
}

impl std::fmt::Display for McpAuthorizationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MCP server {} requires authorization; {}",
            self.server,
            self.guidance()
        )
    }
}
impl std::fmt::Debug for McpAuthorizationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for McpAuthorizationRequired {}

/// Connected MCP servers and the Pi tools discovered from them.
///
/// Clones share transport ownership; [`Self::plugin`] creates the lightweight
/// generation-local adapter that registers the discovered tools.
#[derive(Clone)]
pub struct McpToolSet {
    inner: Arc<McpPool>,
}

impl std::fmt::Debug for McpToolSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolSet")
            .field("servers", &self.inner.servers.len())
            .field("tools", &self.inner.tools.len())
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct McpOfferedTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) exposure: McpExposure,
}

struct McpPool {
    inventory: BTreeMap<String, Vec<McpOfferedTool>>,
    authorization_required: Vec<McpAuthorizationRequired>,
    servers: Vec<Arc<ConnectedServer>>,
    tools: Vec<Arc<McpTool>>,
    resource_tools: Vec<Arc<dyn Tool>>,
}

pub(crate) struct ConnectedServer {
    pub(crate) name: String,
    instructions: Option<String>,
    policy: McpServerPolicy,
    handler: McpClientHandler,
    peer: Peer<RoleClient>,
    service: Mutex<Option<RunningService<RoleClient, McpClientHandler>>>,
}

impl McpToolSet {
    /// Starts every configured server and discovers its complete tool catalog.
    /// If any server fails, already-started services are dropped and cancelled.
    pub async fn connect(configs: Vec<McpServerConfig>) -> Result<Self, McpError> {
        Self::connect_with_auth(configs, McpClientAuth::default()).await
    }

    /// Connects using host-supplied provider and OAuth credential dependencies.
    pub async fn connect_with_auth(
        configs: Vec<McpServerConfig>,
        auth: McpClientAuth,
    ) -> Result<Self, McpError> {
        Self::connect_with_policies_and_auth(
            configs
                .into_iter()
                .map(|config| (config, McpServerPolicy::direct()))
                .collect(),
            auth,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn connect_with_policies(
        configs: Vec<(McpServerConfig, McpServerPolicy)>,
    ) -> Result<Self, McpError> {
        Self::connect_with_policies_and_auth(configs, McpClientAuth::default()).await
    }

    pub(crate) async fn connect_with_policies_and_auth(
        configs: Vec<(McpServerConfig, McpServerPolicy)>,
        auth: McpClientAuth,
    ) -> Result<Self, McpError> {
        Self::connect_generation(configs, auth, false).await
    }

    pub(crate) fn authorization_required(&self) -> &[McpAuthorizationRequired] {
        &self.inner.authorization_required
    }

    /// Product preparation may keep the control plane reachable for auth-only
    /// failures. Invalid configuration and all other connection failures remain fatal.
    pub(crate) async fn connect_generation(
        configs: Vec<(McpServerConfig, McpServerPolicy)>,
        auth: McpClientAuth,
        allow_pending_authorization: bool,
    ) -> Result<Self, McpError> {
        validate_configs(
            &configs
                .iter()
                .map(|(config, _)| config.clone())
                .collect::<Vec<_>>(),
        )?;
        let mut servers = Vec::with_capacity(configs.len());
        let mut inventory = BTreeMap::new();
        let mut authorization_required = Vec::new();
        let mut tools = Vec::new();
        let mut names = HashSet::new();

        for (config, policy) in configs {
            let name = config.name.clone();
            let server = tokio::time::timeout(
                CONNECT_TIMEOUT,
                connect_server(config, policy, auth.clone()),
            )
            .await
            .map_err(|_| McpError::Timeout(name.clone()))?;
            let server = match server {
                Ok(server) => server,
                Err(McpError::AuthorizationRequired(required)) if allow_pending_authorization => {
                    authorization_required.push(required);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let remote_tools = if server
                .peer
                .peer_info()
                .is_some_and(|info| info.capabilities.tools.is_some())
            {
                tokio::time::timeout(CONNECT_TIMEOUT, server.peer.list_all_tools())
                    .await
                    .map_err(|_| McpError::Timeout(name))?
                    .map_err(|_| McpError::ListTools {
                        server: server.name.clone(),
                        message: "server rejected discovery or the connection closed".into(),
                    })?
            } else {
                Vec::new()
            };
            inventory.insert(
                server.name.clone(),
                remote_tools
                    .iter()
                    .map(|tool| McpOfferedTool {
                        name: tool.name.to_string(),
                        description: tool.description.as_deref().unwrap_or_default().to_owned(),
                        exposure: server.policy.exposure_for(&tool.name),
                    })
                    .collect(),
            );
            // Assign before filtering so hidden collision peers cannot change visible names.
            let mut plain_counts = HashMap::new();
            let mut remote_names = HashSet::new();
            for remote in &remote_tools {
                if !remote_names.insert(remote.name.to_string()) {
                    return Err(McpError::DuplicateTool(format!(
                        "{}/{}",
                        server.name, remote.name
                    )));
                }
                *plain_counts
                    .entry(qualified_tool_name(&server.name, &remote.name, |_| false))
                    .or_insert(0) += 1;
            }
            for remote in remote_tools {
                let name = qualified_tool_name(&server.name, &remote.name, |candidate| {
                    names.contains(candidate)
                        || plain_counts.get(candidate).is_some_and(|count| *count > 1)
                });
                let hidden = server.policy.exposure_for(&remote.name) == McpExposure::Hidden;
                let tool = Arc::new(McpTool::new(Arc::clone(&server), remote, name));
                if !names.insert(tool.descriptor.name.clone()) {
                    return Err(McpError::DuplicateTool(tool.descriptor.name.clone()));
                }
                if !hidden {
                    tools.push(tool);
                }
            }
            servers.push(server);
        }

        let resource_tools = create_resource_tools(
            servers
                .iter()
                .filter(|server| server.resources_visible())
                .cloned()
                .collect(),
        );
        Ok(Self {
            inner: Arc::new(McpPool {
                inventory,
                authorization_required,
                servers,
                tools,
                resource_tools,
            }),
        })
    }

    /// Returns a fresh plugin wrapper backed by the connected client pool.
    pub fn plugin(&self) -> Arc<dyn Plugin> {
        self.plugin_with_tool_search_registration(true)
    }

    pub(crate) fn plugin_with_tool_search_registration(
        &self,
        register_tool_search: bool,
    ) -> Arc<dyn Plugin> {
        Arc::new(McpToolPlugin {
            pool: self.clone(),
            register_tool_search,
        })
    }

    /// Descriptors of exposed server tools. Shared resource tools are registered by the plugin.
    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        self.inner
            .tools
            .iter()
            .map(|tool| tool.descriptor.clone())
            .collect()
    }

    pub(crate) fn tool_names(&self) -> Vec<String> {
        self.inner
            .tools
            .iter()
            .map(|tool| tool.descriptor.name.clone())
            .chain(
                self.inner
                    .resource_tools
                    .iter()
                    .map(|tool| tool.spec().name),
            )
            .chain(self.needs_tool_search().then(|| "tool_search".to_string()))
            .collect()
    }

    fn needs_tool_search(&self) -> bool {
        self.inner
            .servers
            .iter()
            .any(|server| server.policy.uses_deferred())
    }

    fn resource_discovery(&self) -> ToolDiscovery {
        let direct = self.inner.servers.iter().any(|server| {
            server.resources_visible() && server.policy.exposure == McpExposure::Direct
        });
        ToolDiscovery {
            origin: pi_plugin::ToolOrigin::Mcp,
            exposure: if direct {
                ToolExposure::Direct
            } else {
                ToolExposure::Deferred
            },
            namespace: Some(ToolNamespace {
                name: "mcp_resources".into(),
                aliases: Vec::new(),
                description: Some("Resources provided by MCP servers".into()),
                instructions: Some(
                    self.inner
                        .servers
                        .iter()
                        .filter(|server| server.resources_visible())
                        .filter_map(|server| {
                            server.instructions.as_ref().map(|instructions| {
                                format!(
                                    "Instructions for MCP resources from `{}`:\n{instructions}",
                                    server.name
                                )
                            })
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                ),
            }),
        }
    }

    pub(crate) fn has_resources(&self, name: &str) -> bool {
        self.inner
            .servers
            .iter()
            .any(|server| server.name == name && server.resources_visible())
    }

    /// Read-only inventory, including tools deliberately omitted from the registry.
    pub(crate) fn offered_tools(&self, server: &str) -> &[McpOfferedTool] {
        self.inner
            .inventory
            .get(server)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn tool_exposure_counts(&self, server: &str) -> (usize, usize, usize) {
        let mut direct = 0;
        let mut deferred = 0;
        let mut codemode = 0;
        for tool in self
            .inner
            .tools
            .iter()
            .filter(|tool| tool.server.name == server)
        {
            match tool.server.policy.exposure_for(&tool.remote_name) {
                McpExposure::Direct => direct += 1,
                McpExposure::Deferred => deferred += 1,
                McpExposure::Codemode => codemode += 1,
                McpExposure::Hidden => {}
            }
        }
        (direct, deferred, codemode)
    }

    pub(crate) fn prompt_patch(&self, event: BeforeAgentStartEvent) -> BeforeAgentStartPatch {
        let mut prompt = event.system_prompt;
        let base_length = prompt.len();
        // Only enabled servers enter the pool. Like Pi, directory reach follows their
        // configured exposure, independently of the current active entry points.
        let section = render_servers_section(
            self.inner
                .servers
                .iter()
                .filter(|server| server.policy.uses_codemode() || server.policy.uses_deferred())
                .map(|server| {
                    (
                        server.name.as_str(),
                        server.policy.description.as_deref(),
                        server.instructions.as_deref(),
                        if server.policy.uses_codemode() {
                            "codemode"
                        } else {
                            "tool_search"
                        },
                    )
                }),
        );
        let changed = section.is_some();
        if let Some(section) = section {
            prompt.push_str(&format!("\n\n<mcp_servers>\n{section}\n</mcp_servers>"));
        }
        BeforeAgentStartPatch {
            prompt_state: None,
            prompt_contributions: if changed {
                vec![pi_core::PromptContribution {
                    category: pi_core::ContextCategory::McpTools,
                    name: "MCP server directory".into(),
                    text: prompt[base_length..].to_string(),
                }]
            } else {
                Vec::new()
            },
            system_prompt: changed.then_some(prompt),
            ..BeforeAgentStartPatch::default()
        }
    }

    /// Closes all transports and waits for child-process cleanup.
    pub async fn shutdown(&self) -> Result<(), McpError> {
        let mut first_error = None;
        for server in &self.inner.servers {
            let Some(mut service) = server.service.lock().await.take() else {
                continue;
            };
            match service.close_with_timeout(SHUTDOWN_TIMEOUT).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    first_error.get_or_insert_with(|| McpError::Shutdown {
                        server: server.name.clone(),
                        message: "cleanup timed out".to_string(),
                    });
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| McpError::Shutdown {
                        server: server.name.clone(),
                        message: error.to_string(),
                    });
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

async fn connect_server(
    config: McpServerConfig,
    policy: McpServerPolicy,
    client_auth: McpClientAuth,
) -> Result<Arc<ConnectedServer>, McpError> {
    let McpServerConfig {
        name,
        transport,
        oauth,
        auth,
    } = config;
    let handler = McpClientHandler::new(policy.root.as_deref());
    let auto = ClientLifecycleMode::Auto {
        preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
        legacy_version: Some(rmcp::model::ProtocolVersion::LATEST),
    };
    let service = match connect_once(
        &name,
        &transport,
        oauth.as_ref(),
        auth.as_ref(),
        &client_auth,
        handler.clone(),
        auto,
    )
    .await
    {
        Ok(service) => service,
        // Some deployed legacy servers return an uncorrelated JSON-RPC error
        // to server/discover. A fresh transport can still complete the stable
        // initialize handshake; retrying cannot reuse ambiguous transport state.
        Err(McpError::Initialize { .. }) => {
            connect_once(
                &name,
                &transport,
                oauth.as_ref(),
                auth.as_ref(),
                &client_auth,
                handler.clone(),
                ClientLifecycleMode::Initialize,
            )
            .await?
        }
        Err(error) => return Err(error),
    };
    Ok(connected_server(name, policy, handler, service))
}

async fn connect_once(
    name: &str,
    config: &McpTransport,
    oauth: Option<&McpOAuthConfig>,
    provider_auth: Option<&McpProviderAuth>,
    client_auth: &McpClientAuth,
    handler: McpClientHandler,
    lifecycle: ClientLifecycleMode,
) -> Result<RunningService<RoleClient, McpClientHandler>, McpError> {
    match config {
        McpTransport::Http { url, headers } => {
            // reqwest 0.13's no-provider feature requires an explicit default.
            // Respect an embedding host's choice, otherwise match Pi's ring backend.
            if rustls::crypto::CryptoProvider::get_default().is_none() {
                let _ = rustls::crypto::ring::default_provider().install_default();
            }
            let headers = http_headers(name, headers)?;
            let has_static_authorization = headers.contains_key(&http::header::AUTHORIZATION);
            let transport_config =
                StreamableHttpClientTransportConfig::with_uri(url.clone()).custom_headers(headers);
            if let Some(provider_auth) = provider_auth.filter(|_| !has_static_authorization) {
                let resolver =
                    client_auth
                        .provider_tokens
                        .clone()
                        .ok_or_else(|| McpError::Initialize {
                            server: name.to_string(),
                            message: "provider authentication resolver is not installed".into(),
                        })?;
                let transport = StreamableHttpClientTransport::with_client(
                    ProviderAuthClient::new(provider_auth.provider.clone(), resolver),
                    transport_config,
                );
                handler.serve_with_lifecycle(transport, lifecycle).await
            } else if !has_static_authorization {
                let oauth_client = oauth_http_client(name, url, oauth, client_auth)
                    .await
                    .map_err(|_| McpError::Initialize {
                        server: name.to_string(),
                        message: "OAuth credential state could not be loaded".into(),
                    })?;
                let transport =
                    StreamableHttpClientTransport::with_client(oauth_client, transport_config);
                handler.serve_with_lifecycle(transport, lifecycle).await
            } else {
                let transport = StreamableHttpClientTransport::from_config(transport_config);
                handler.serve_with_lifecycle(transport, lifecycle).await
            }
        }
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut process = tokio::process::Command::new(command);
            process.args(args).envs(env);
            if let Some(cwd) = cwd {
                process.current_dir(cwd);
            }
            // Child logs must not corrupt the TUI / protocol channel or expose
            // credentials. Protocol failures are surfaced through typed errors.
            let (transport, _) = TokioChildProcess::builder(process)
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|error| McpError::Start {
                    server: name.to_string(),
                    message: error.to_string(),
                })?;
            handler.serve_with_lifecycle(transport, lifecycle).await
        }
    }
    .map_err(|error| {
        if error.is_authorization_required() {
            McpError::AuthorizationRequired(McpAuthorizationRequired {
                server: name.to_string(),
                provider: provider_auth.map(|auth| auth.provider.clone()),
                static_authorization: matches!(config, McpTransport::Http { headers, .. }
                    if headers.keys().any(|key| key.eq_ignore_ascii_case("authorization"))),
                challenge: error.auth_challenge().map(str::to_owned),
            })
        } else {
            McpError::Initialize {
                server: name.to_string(),
                message: "handshake failed; check endpoint, credentials and server availability"
                    .into(),
            }
        }
    })
}

fn connected_server(
    name: String,
    policy: McpServerPolicy,
    handler: McpClientHandler,
    service: RunningService<RoleClient, McpClientHandler>,
) -> Arc<ConnectedServer> {
    let instructions = service
        .peer()
        .peer_info()
        .and_then(|info| info.instructions.clone())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Arc::new(ConnectedServer {
        name,
        instructions,
        policy,
        handler,
        peer: service.peer().clone(),
        service: Mutex::new(Some(service)),
    })
}

pub fn validate_configs(configs: &[McpServerConfig]) -> Result<(), McpError> {
    let mut names = HashSet::new();
    let mut namespaces = HashSet::new();
    for config in configs {
        let name = config.name.trim();
        if name.is_empty() {
            return Err(McpError::EmptyServerName);
        }
        if config
            .name
            .bytes()
            .any(|c| !c.is_ascii_alphanumeric() && c != b'_' && c != b'-')
        {
            return Err(McpError::InvalidServerName(config.name.clone()));
        }
        if !names.insert(name.to_string()) {
            return Err(McpError::DuplicateServerName(name.to_string()));
        }
        if !namespaces.insert(identifier_component(name)) {
            return Err(McpError::DuplicateNamespace(name.to_string()));
        }
        match &config.transport {
            McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                return Err(McpError::EmptyCommand(name.to_string()));
            }
            McpTransport::Stdio { .. } if config.oauth.is_some() || config.auth.is_some() => {
                return Err(McpError::InvalidHttp {
                    server: name.into(),
                    message: "oauth and auth are only valid for HTTP servers",
                });
            }
            McpTransport::Http { url, headers } => {
                let parsed = url::Url::parse(url).map_err(|_| McpError::InvalidHttp {
                    server: name.into(),
                    message: "expected an absolute http(s) URL",
                })?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(McpError::InvalidHttp {
                        server: name.into(),
                        message: "use http(s), no URL userinfo or fragment; put authentication in headers",
                    });
                }
                http_headers(name, headers)?;
                validate_http_auth(name, &parsed, config.oauth.as_ref(), config.auth.as_ref())?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_http_auth(
    name: &str,
    server_url: &url::Url,
    oauth: Option<&McpOAuthConfig>,
    auth: Option<&McpProviderAuth>,
) -> Result<(), McpError> {
    let invalid = |message| McpError::InvalidHttp {
        server: name.into(),
        message,
    };
    let loopback = matches!(
        server_url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1")
    );
    if let Some(auth) = auth {
        if auth.provider.trim().is_empty() {
            return Err(invalid("auth.provider must not be empty"));
        }
        if server_url.scheme() != "https" && !loopback {
            return Err(invalid("provider auth requires https or a loopback URL"));
        }
    }
    let Some(oauth) = oauth else {
        return Ok(());
    };
    if server_url.scheme() != "https" && !loopback {
        return Err(invalid("OAuth requires https or a loopback URL"));
    }
    if oauth.client_id.as_ref().is_some_and(String::is_empty) {
        return Err(invalid("oauth.clientId must not be empty"));
    }
    if oauth.callback_port == Some(0) {
        return Err(invalid("oauth.callbackPort must not be zero"));
    }
    if oauth.client_registration == Some(McpOAuthClientRegistration::Cimd)
        && (oauth.client_id.is_some() || oauth.client_name.is_some())
    {
        return Err(invalid(
            "CIMD cannot be combined with oauth.clientId or oauth.clientName",
        ));
    }
    if let Some(callback) = &oauth.callback_url {
        let callback = url::Url::parse(callback)
            .map_err(|_| invalid("oauth.callbackUrl must be a loopback http URL"))?;
        if callback.scheme() != "http"
            || !callback.username().is_empty()
            || callback.password().is_some()
            || !matches!(callback.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
            || callback.query().is_some()
            || callback.fragment().is_some()
        {
            return Err(invalid("oauth.callbackUrl must be a loopback http URL"));
        }
        if oauth
            .callback_port
            .is_some_and(|port| callback.port().is_some_and(|value| value != port))
        {
            return Err(invalid("oauth callback port settings disagree"));
        }
    }
    if let Some(metadata) = &oauth.auth_server_metadata_url {
        let metadata = url::Url::parse(metadata)
            .map_err(|_| invalid("oauth auth-server metadata URL is invalid"))?;
        if !metadata.username().is_empty()
            || metadata.password().is_some()
            || metadata.fragment().is_some()
            || (metadata.scheme() != "https"
                && !(metadata.scheme() == "http"
                    && matches!(metadata.host_str(), Some("localhost" | "127.0.0.1" | "::1"))))
        {
            return Err(invalid("oauth auth-server metadata URL must be secure"));
        }
    }
    Ok(())
}

fn http_headers(
    name: &str,
    headers: &BTreeMap<String, String>,
) -> Result<HashMap<http::HeaderName, http::HeaderValue>, McpError> {
    let invalid = || McpError::InvalidHttp {
        server: name.into(),
        message: "invalid, duplicate or protocol-owned header",
    };
    let mut result = HashMap::new();
    for (key, value) in headers {
        let key: http::HeaderName = key.parse().map_err(|_| invalid())?;
        if matches!(
            key.as_str(),
            "host"
                | "content-length"
                | "content-type"
                | "accept"
                | "connection"
                | "transfer-encoding"
                | "mcp-session-id"
                | "mcp-protocol-version"
                | "last-event-id"
        ) {
            return Err(invalid());
        }
        let mut value: http::HeaderValue = value.parse().map_err(|_| invalid())?;
        value.set_sensitive(true);
        if result.insert(key, value).is_some() {
            return Err(invalid());
        }
    }
    Ok(result)
}

struct McpToolPlugin {
    pool: McpToolSet,
    register_tool_search: bool,
}

#[pi_plugin::plugin]
impl Plugin for McpToolPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("mcp")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        for tool in &self.pool.inner.tools {
            context.register_tool_with_discovery(
                tool.clone(),
                ToolDiscovery {
                    origin: pi_plugin::ToolOrigin::Mcp,
                    exposure: if matches!(
                        tool.server.policy.exposure_for(&tool.remote_name),
                        McpExposure::Deferred | McpExposure::Codemode
                    ) {
                        ToolExposure::Deferred
                    } else {
                        ToolExposure::Direct
                    },
                    namespace: Some(ToolNamespace {
                        name: format!("mcp__{}", identifier_component(&tool.server.name)),
                        aliases: vec![tool.server.name.clone()],
                        description: tool.server.policy.description.clone(),
                        instructions: tool.server.instructions.clone(),
                    }),
                },
            )?;
        }
        for tool in &self.pool.inner.resource_tools {
            context.register_tool_with_discovery(tool.clone(), self.pool.resource_discovery())?;
        }
        if self.register_tool_search && self.pool.needs_tool_search() {
            context.register_tool(pi_plugin_tool_search::tool_search_tool())?;
        }
        Ok(())
    }

    async fn before_agent_start(
        &self,
        _context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        Ok(self.pool.prompt_patch(event))
    }
}

struct McpTool {
    server: Arc<ConnectedServer>,
    remote_name: String,
    descriptor: McpToolDescriptor,
    output_schema: Value,
    execution_mode: ToolExecutionMode,
}

impl McpTool {
    fn new(server: Arc<ConnectedServer>, remote: McpToolSpec, name: String) -> Self {
        // Read-only hints affect scheduling only, never authorization. Unknown
        // or mutating tools must finish before subsequent calls can observe them.
        let execution_mode = if remote
            .annotations
            .as_ref()
            .is_some_and(|annotations| annotations.read_only_hint == Some(true))
        {
            ToolExecutionMode::Parallel
        } else {
            ToolExecutionMode::Sequential
        };
        let remote_name = remote.name.into_owned();
        let title = remote
            .title
            .or_else(|| {
                remote
                    .annotations
                    .as_ref()
                    .and_then(|value| value.title.clone())
            })
            .filter(|title| !title.is_empty());
        let label = format!("{}/{remote_name}", server.name);
        let description = remote
            .description
            .map(|description| description.trim().to_string())
            .filter(|description| !description.is_empty())
            .or(title)
            .unwrap_or_else(|| format!("MCP tool {remote_name} from server {}", server.name));
        let output_schema = crate::output_schema::result_schema(
            remote
                .output_schema
                .map(|schema| Value::Object((*schema).clone())),
        );
        let parameters = normalize_input_schema(Value::Object((*remote.input_schema).clone()));
        Self {
            descriptor: McpToolDescriptor {
                server_name: server.name.clone(),
                remote_name: remote_name.clone(),
                name,
                label,
                description,
                parameters,
            },
            server,
            remote_name,
            output_schema,
            execution_mode,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn structured_output(&self, result: &ToolResult) -> Option<Value> {
        // Respect hook replacement/invalidation of the typed CallToolResult.
        result.structured_content.clone()
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            output_schema: Some(self.output_schema.clone()),
            constrained_sampling: None,
            name: self.descriptor.name.clone(),
            label: self.descriptor.label.clone(),
            description: self.descriptor.description.clone(),
            parameters: self.descriptor.parameters.clone(),
            execution_mode: self.execution_mode,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let Value::Object(arguments) = input else {
            return Err(ToolError::InvalidArguments(
                "arguments must be a JSON object".to_string(),
            ));
        };
        let response = self
            .server
            .request(
                &context,
                ClientRequest::CallToolRequest(CallToolRequest::new(
                    CallToolRequestParams::new(self.remote_name.clone()).with_arguments(arguments),
                )),
                &self.remote_name,
                &updates,
            )
            .await
            .map_err(|error| error.into_tool_error(&self.server.name))?;
        let ServerResult::CallToolResult(result) = response else {
            return Err(
                McpRequestError::Service(rmcp::service::ServiceError::UnexpectedResponse)
                    .into_tool_error(&self.server.name),
            );
        };
        let readable_resources = self.server.resources_visible()
            && context
                .session
                .active_tools()
                .is_ok_and(|tools| tools.iter().any(|name| name == "read_mcp_resource"));
        Ok(project_result_with_resources(
            &self.server.name,
            &self.remote_name,
            result,
            readable_resources,
        ))
    }
}

/// Keep protocol classification private while omitting endpoint/credential details from errors.
#[derive(Debug)]
pub(crate) enum McpRequestError {
    Aborted,
    Timeout(Duration),
    Service(rmcp::service::ServiceError),
}

impl McpRequestError {
    pub(crate) fn is_method_not_found(&self) -> bool {
        matches!(self, Self::Service(rmcp::service::ServiceError::McpError(error))
            if error.code == rmcp::model::ErrorCode::METHOD_NOT_FOUND)
    }

    pub(crate) fn into_tool_error(self, server: &str) -> ToolError {
        match self {
            Self::Aborted => ToolError::Aborted,
            Self::Timeout(timeout) => ToolError::Execution(format!(
                "MCP request timed out for server {server} after {} seconds without progress",
                timeout.as_secs_f64(),
            )),
            Self::Service(_) => {
                ToolError::Execution(format!("MCP request failed for server {server}"))
            }
        }
    }
}

impl ConnectedServer {
    fn resources_visible(&self) -> bool {
        self.policy.exposure != McpExposure::Hidden
            && self
                .peer
                .peer_info()
                .is_some_and(|info| info.capabilities.resources.is_some())
    }

    pub(crate) async fn request(
        &self,
        context: &ToolContext,
        request: ClientRequest,
        tool: &str,
        updates: &ToolUpdateSink,
    ) -> Result<ServerResult, McpRequestError> {
        context
            .signal()
            .check()
            .map_err(|_| McpRequestError::Aborted)?;
        // Subscribe before sending: a fast server may report progress immediately.
        let mut progress = self.handler.progress.subscribe();
        let timeout = self.policy.timeout;
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut request = tokio::select! {
            result = self.peer.send_cancellable_request(request, Default::default()) => result.map_err(McpRequestError::Service)?,
            () = context.signal().wait() => return Err(McpRequestError::Aborted),
            () = &mut deadline => return Err(McpRequestError::Timeout(timeout)),
        };
        loop {
            tokio::select! {
                biased;
                () = context.signal().wait() => {
                    let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, request.cancel(Some("cancelled by user".into()))).await;
                    return Err(McpRequestError::Aborted);
                }
                result = &mut request.rx => return result
                    .map_err(|_| McpRequestError::Service(rmcp::service::ServiceError::TransportClosed))?
                    .map_err(McpRequestError::Service),
                () = &mut deadline => {
                    let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, request.cancel(Some("request timeout".into()))).await;
                    return Err(McpRequestError::Timeout(timeout));
                }
                notification = progress.recv() => {
                    if let Ok(notification) = notification
                        && notification.progress_token == request.progress_token
                    {
                        deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                        let text = notification.message.unwrap_or_else(|| match notification.total {
                            Some(total) => format!("Progress {}/{total}", notification.progress),
                            None => format!("Progress {}", notification.progress),
                        });
                        updates.send(ToolUpdate {
                            content: vec![ContentBlock::Text(TextContent::new(text))],
                            details: Some(serde_json::json!({"server": self.name, "tool": tool})),
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "client_contract_tests.rs"]
mod contract_tests;

#[cfg(test)]
mod tests {
    use rmcp::ServiceExt;
    use rmcp::{
        ServerHandler,
        handler::server::{router::tool::ToolRouter, wrapper::Parameters},
        model::{ServerCapabilities, ServerInfo},
        schemars, tool, tool_handler, tool_router,
    };

    use super::*;

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct EchoRequest {
        text: String,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    struct EchoServer {
        tool_router: ToolRouter<Self>,
    }

    #[tool_router]
    impl EchoServer {
        fn new() -> Self {
            Self {
                tool_router: Self::tool_router(),
            }
        }

        #[tool(description = "Echo text from a deterministic MCP server")]
        fn echo(&self, Parameters(request): Parameters<EchoRequest>) -> String {
            request.text
        }
    }

    #[tool_handler]
    impl ServerHandler for EchoServer {
        fn get_info(&self) -> ServerInfo {
            let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
            info.instructions = Some("  Echo server usage guidance.  ".into());
            info
        }
    }

    #[tokio::test]
    async fn discovers_and_invokes_tools_over_an_mcp_transport() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            let server = EchoServer::new().serve(server_io).await.unwrap();
            server.waiting().await.unwrap()
        });
        let handler = McpClientHandler::new(None);
        let client = handler.clone().serve(client_io).await.unwrap();
        let connected = connected_server(
            "fixture-server".to_string(),
            McpServerPolicy::default(),
            handler,
            client,
        );
        let remote = connected.peer.list_all_tools().await.unwrap().remove(0);
        let tool = McpTool::new(
            Arc::clone(&connected),
            remote,
            "mcp__fixture_server__echo".into(),
        );

        assert_eq!(tool.spec().name, "mcp__fixture_server__echo");
        assert_eq!(tool.spec().execution_mode, ToolExecutionMode::Sequential);
        let (_abort, signal) = pi_core::AbortHandle::new();
        let (updates, _receiver) = ToolUpdateSink::channel();
        let result = tool
            .execute(
                ToolContext::standalone(PathBuf::from("/tmp"), signal.clone()),
                "call-1".into(),
                serde_json::json!({"text":"hello"}),
                updates,
            )
            .await
            .unwrap();

        assert_eq!(
            result.content,
            vec![ContentBlock::Text(TextContent::new("hello"))]
        );
        assert!(!result.is_error);
        let pool = McpToolSet {
            inner: Arc::new(McpPool {
                servers: vec![Arc::clone(&connected)],
                tools: vec![Arc::new(tool)],
                resource_tools: Vec::new(),
                inventory: BTreeMap::new(),
                authorization_required: Vec::new(),
            }),
        };
        let (driver, registry) = pi_plugin::RegistriesBuilder::new()
            .register_plugins(vec![pool.plugin()])
            .unwrap();
        assert_eq!(
            registry.tool_infos()[0]
                .discovery
                .namespace
                .as_ref()
                .unwrap()
                .instructions
                .as_deref(),
            Some("Echo server usage guidance.")
        );
        assert_eq!(
            registry.tool_infos()[0]
                .discovery
                .namespace
                .as_ref()
                .unwrap()
                .aliases,
            ["fixture-server"]
        );
        for enabled in [true, true, false] {
            let patch = driver
                .before_agent_start(
                    &pi_core::RunId::next(),
                    std::path::Path::new("/tmp"),
                    &signal,
                    BeforeAgentStartEvent {
                        prompt_state: None,
                        system_prompt: "base".into(),
                        input_messages: vec![],
                        active_tools: if enabled {
                            vec!["mcp__fixture_server__echo".into()]
                        } else {
                            vec![]
                        },
                        provider_id: pi_core::ProviderId::new("scripted"),
                        model_id: pi_core::ModelId::new("test"),
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                patch.system_prompt.as_deref(),
                Some(
                    "base\n\n<mcp_servers>\nMCP servers whose tools are not declared to you. Call the tools of `codemode` servers from codemode scripts.\n- mcp__fixture_server (codemode): Echo server usage guidance.\n</mcp_servers>"
                )
            );
        }
        connected
            .service
            .lock()
            .await
            .take()
            .unwrap()
            .cancel()
            .await
            .unwrap();
        server_task.await.unwrap();
    }

    #[test]
    fn validates_server_identity_before_starting_processes() {
        assert!(matches!(
            validate_configs(&[McpServerConfig::stdio("", "node")]),
            Err(McpError::EmptyServerName)
        ));
        assert!(matches!(
            validate_configs(&[
                McpServerConfig::stdio("one", "node"),
                McpServerConfig::stdio("one", "node"),
            ]),
            Err(McpError::DuplicateServerName(name)) if name == "one"
        ));
    }

    #[tokio::test]
    async fn streamable_http_discovers_and_calls_over_json_and_sse() {
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        };
        for json_response in [true, false] {
            let config = StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(json_response);
            let cancellation = config.cancellation_token.clone();
            let service: StreamableHttpService<EchoServer, LocalSessionManager> =
                StreamableHttpService::new(|| Ok(EchoServer::new()), Default::default(), config);
            let router =
                axum::Router::new()
                    .nest_service("/mcp", service)
                    .layer(axum::middleware::from_fn(
                        |request: axum::extract::Request, next: axum::middleware::Next| async move {
                            if request
                                .headers()
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                != Some("Bearer fixture-token")
                            {
                                return axum::response::IntoResponse::into_response(
                                    http::StatusCode::UNAUTHORIZED,
                                );
                            }
                            next.run(request).await
                        },
                    ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let config = McpServerConfig::http("remote", &url).headers(BTreeMap::from([(
                "Authorization".into(),
                "Bearer fixture-token".into(),
            )]));
            let pool = McpToolSet::connect(vec![config]).await.unwrap();
            assert_eq!(pool.tools()[0].name, "mcp__remote__echo");
            let (_, signal) = pi_core::AbortHandle::new();
            let (updates, _) = ToolUpdateSink::channel();
            let result = pool.inner.tools[0]
                .execute(
                    ToolContext::standalone(PathBuf::from("/tmp"), signal),
                    "call".into(),
                    serde_json::json!({"text":"http echo"}),
                    updates,
                )
                .await
                .unwrap();
            assert_eq!(
                result.content,
                vec![ContentBlock::Text(TextContent::new("http echo"))]
            );
            pool.shutdown().await.unwrap();
            let error = McpToolSet::connect(vec![McpServerConfig::http(
                "remote",
                format!("{url}?secret=do-not-log"),
            )])
            .await
            .unwrap_err()
            .to_string();
            assert!(!error.contains("do-not-log"));
            assert!(error.contains("handshake failed"));
            cancellation.cancel();
            task.abort();
            let _ = task.await;
        }
    }

    #[test]
    fn http_validation_and_debug_do_not_expose_secrets() {
        for url in [
            "file:///tmp/mcp",
            "https://user:secret@example.com/mcp",
            "not a url",
        ] {
            assert!(validate_configs(&[McpServerConfig::http("test", url)]).is_err());
        }
        let config = McpServerConfig::http("test", "https://example.com/mcp?token=secret").headers(
            BTreeMap::from([("Authorization".into(), "Bearer secret".into())]),
        );
        assert!(validate_configs(std::slice::from_ref(&config)).is_ok());
        assert!(!format!("{config:?}").contains("secret"));
        for key in ["Host", "Accept", "Mcp-Session-Id", "bad header"] {
            assert!(
                validate_configs(&[config
                    .clone()
                    .headers(BTreeMap::from([(key.into(), "secret".into())]))])
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn falls_back_to_legacy_initialize_over_streamable_http() {
        use axum::response::IntoResponse;
        use serde_json::json;
        let router = axum::Router::new().route("/mcp", axum::routing::post(|axum::Json(request): axum::Json<Value>| async move {
            let id = &request["id"];
            if id.is_null() { return http::StatusCode::ACCEPTED.into_response(); }
            let result = match request["method"].as_str().unwrap() {
                "server/discover" => return axum::Json(json!({"jsonrpc":"2.0", "id":"server-error", "error":{"code":-32602,"message":"Invalid request parameters"}})).into_response(),
                "initialize" => json!({"protocolVersion":"2025-03-26", "capabilities":{"tools":{}}, "serverInfo":{"name":"legacy","version":"1"}}),
                "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                "tools/call" => json!({"content":[{"type":"text","text":"legacy response"}]}),
                method => panic!("unexpected method {method}"),
            };
            axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = McpServerConfig::http(
            "legacy",
            format!("http://{}/mcp", listener.local_addr().unwrap()),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let pool = McpToolSet::connect(vec![config]).await.unwrap();
        let (_, signal) = pi_core::AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = pool.inner.tools[0]
            .execute(
                ToolContext::standalone(PathBuf::from("/tmp"), signal),
                "call".into(),
                json!({}),
                updates,
            )
            .await
            .unwrap();
        assert_eq!(
            result.content,
            vec![ContentBlock::Text(TextContent::new("legacy response"))]
        );
        pool.shutdown().await.unwrap();
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn abort_closes_the_remote_stream_not_only_the_local_wait() {
        use rmcp::transport::streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        };
        #[derive(Clone)]
        struct Probe {
            started: Arc<tokio::sync::Notify>,
            cancelled: Arc<tokio::sync::Notify>,
        }
        impl ServerHandler for Probe {
            fn get_info(&self) -> ServerInfo {
                ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            }
            async fn call_tool(
                &self,
                _: CallToolRequestParams,
                context: rmcp::service::RequestContext<rmcp::RoleServer>,
            ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
                self.started.notify_one();
                context.ct.cancelled().await;
                self.cancelled.notify_one();
                Ok(rmcp::model::CallToolResult::success(vec![]).into())
            }
        }
        let probe = Probe {
            started: Arc::new(tokio::sync::Notify::new()),
            cancelled: Arc::new(tokio::sync::Notify::new()),
        };
        let server_probe = probe.clone();
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_sse_keep_alive(Some(Duration::from_millis(20)));
        let ct = config.cancellation_token.clone();
        let service: StreamableHttpService<Probe, LocalSessionManager> = StreamableHttpService::new(
            move || Ok(server_probe.clone()),
            Default::default(),
            config,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = McpServerConfig::http(
            "probe",
            format!("http://{}/mcp", listener.local_addr().unwrap()),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().nest_service("/mcp", service))
                .await
                .unwrap();
        });
        let server = connect_server(config, McpServerPolicy::default(), McpClientAuth::default())
            .await
            .unwrap();
        let tool = McpTool::new(
            server.clone(),
            McpToolSpec::new(
                "wait",
                "Wait",
                Arc::new(serde_json::Map::from_iter([(
                    "type".into(),
                    Value::String("object".into()),
                )])),
            ),
            "mcp__probe__wait".into(),
        );
        let (abort, signal) = pi_core::AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let call = tokio::spawn(async move {
            tool.execute(
                ToolContext::standalone(PathBuf::from("/tmp"), signal),
                "call".into(),
                serde_json::json!({}),
                updates,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), probe.started.notified())
            .await
            .unwrap();
        abort.abort();
        assert!(matches!(call.await.unwrap(), Err(ToolError::Aborted)));
        tokio::time::timeout(Duration::from_secs(3), probe.cancelled.notified())
            .await
            .unwrap();
        drop(server);
        ct.cancel();
        task.abort();
        let _ = task.await;
    }
}
