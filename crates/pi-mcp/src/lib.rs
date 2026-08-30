#![forbid(unsafe_code)]

//! Protocol-neutral MCP client integration for Pi runtime generations.
//!
//! This crate owns MCP transports, discovery, invocation, and cleanup. It has
//! no knowledge of ACP or Pi session persistence; callers adapt their wire
//! configuration into [`McpServerConfig`] and inject [`McpToolSet::plugin`]
//! through their own generation seam.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, ContentBlock, ImageContent, PluginId, RegisterContext, TextContent, Tool,
    ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdateSink,
};
use rmcp::model::{CallToolRequestParams, ContentBlock as McpContentBlock, Tool as McpToolSpec};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{Peer, RoleClient, ServiceExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
}

impl McpServerConfig {
    pub fn stdio(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransport::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            },
        }
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let McpTransport::Stdio {
            args: configured, ..
        } = &mut self.transport;
        *configured = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn env(mut self, env: BTreeMap<String, String>) -> Self {
        let McpTransport::Stdio {
            env: configured, ..
        } = &mut self.transport;
        *configured = env;
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        let McpTransport::Stdio {
            cwd: configured, ..
        } = &mut self.transport;
        *configured = Some(cwd.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
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
    #[error("MCP stdio command for server {0} must not be empty")]
    EmptyCommand(String),
    #[error("failed to start MCP server {server}: {message}")]
    Start { server: String, message: String },
    #[error("failed to initialize MCP server {server}: {message}")]
    Initialize { server: String, message: String },
    #[error("failed to list tools from MCP server {server}: {message}")]
    ListTools { server: String, message: String },
    #[error("duplicate qualified MCP tool name: {0}")]
    DuplicateTool(String),
    #[error("failed to shut down MCP server {server}: {message}")]
    Shutdown { server: String, message: String },
}

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

struct McpPool {
    servers: Vec<Arc<ConnectedServer>>,
    tools: Vec<Arc<McpTool>>,
}

struct ConnectedServer {
    name: String,
    peer: Peer<RoleClient>,
    service: Mutex<Option<RunningService<RoleClient, ()>>>,
}

impl McpToolSet {
    /// Starts every configured server and discovers its complete tool catalog.
    /// If any server fails, already-started services are dropped and cancelled.
    pub async fn connect(configs: Vec<McpServerConfig>) -> Result<Self, McpError> {
        validate_configs(&configs)?;
        let mut servers = Vec::with_capacity(configs.len());
        let mut tools = Vec::new();
        let mut names = HashSet::new();

        for config in configs {
            let server = connect_stdio(config).await?;
            let remote_tools =
                server
                    .peer
                    .list_all_tools()
                    .await
                    .map_err(|error| McpError::ListTools {
                        server: server.name.clone(),
                        message: error.to_string(),
                    })?;
            for remote in remote_tools {
                let tool = Arc::new(McpTool::new(Arc::clone(&server), remote));
                if !names.insert(tool.descriptor.name.clone()) {
                    return Err(McpError::DuplicateTool(tool.descriptor.name.clone()));
                }
                tools.push(tool);
            }
            servers.push(server);
        }

        Ok(Self {
            inner: Arc::new(McpPool { servers, tools }),
        })
    }

    /// Returns a fresh plugin wrapper backed by the connected client pool.
    pub fn plugin(&self) -> Arc<dyn AgentPlugin> {
        Arc::new(McpToolPlugin {
            pool: Arc::clone(&self.inner),
        })
    }

    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        self.inner
            .tools
            .iter()
            .map(|tool| tool.descriptor.clone())
            .collect()
    }

    /// Closes all child transports and waits for process cleanup.
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

async fn connect_stdio(config: McpServerConfig) -> Result<Arc<ConnectedServer>, McpError> {
    let McpServerConfig { name, transport } = config;
    let McpTransport::Stdio {
        command,
        args,
        env,
        cwd,
    } = transport;
    let mut process = tokio::process::Command::new(command);
    process.args(args).envs(env);
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    let transport = TokioChildProcess::new(process).map_err(|error| McpError::Start {
        server: name.clone(),
        message: error.to_string(),
    })?;
    let service =
        ().serve(transport)
            .await
            .map_err(|error| McpError::Initialize {
                server: name.clone(),
                message: error.to_string(),
            })?;
    Ok(connected_server(name, service))
}

fn connected_server(name: String, service: RunningService<RoleClient, ()>) -> Arc<ConnectedServer> {
    Arc::new(ConnectedServer {
        name,
        peer: service.peer().clone(),
        service: Mutex::new(Some(service)),
    })
}

fn validate_configs(configs: &[McpServerConfig]) -> Result<(), McpError> {
    let mut names = HashSet::new();
    for config in configs {
        let name = config.name.trim();
        if name.is_empty() {
            return Err(McpError::EmptyServerName);
        }
        if !names.insert(name.to_string()) {
            return Err(McpError::DuplicateServerName(name.to_string()));
        }
        let McpTransport::Stdio { command, .. } = &config.transport;
        if command.trim().is_empty() {
            return Err(McpError::EmptyCommand(name.to_string()));
        }
    }
    Ok(())
}

struct McpToolPlugin {
    pool: Arc<McpPool>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for McpToolPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("mcp")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for tool in &self.pool.tools {
            context.register_tool(tool.clone())?;
        }
        Ok(())
    }
}

struct McpTool {
    server: Arc<ConnectedServer>,
    remote_name: String,
    descriptor: McpToolDescriptor,
}

impl McpTool {
    fn new(server: Arc<ConnectedServer>, remote: McpToolSpec) -> Self {
        let remote_name = remote.name.into_owned();
        let label = remote
            .title
            .or_else(|| {
                remote
                    .annotations
                    .as_ref()
                    .and_then(|value| value.title.clone())
            })
            .unwrap_or_else(|| remote_name.clone());
        let description = remote
            .description
            .map(|description| description.into_owned())
            .unwrap_or_else(|| format!("MCP tool {}/{}", server.name, remote_name));
        let parameters = Value::Object((*remote.input_schema).clone());
        let name = qualified_tool_name(&server.name, &remote_name);
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
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.descriptor.name.clone(),
            label: self.descriptor.label.clone(),
            description: self.descriptor.description.clone(),
            parameters: self.descriptor.parameters.clone(),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _tool_call_id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let Value::Object(arguments) = input else {
            return Err(ToolError::InvalidArguments(
                "arguments must be a JSON object".to_string(),
            ));
        };
        let request = self.server.peer.call_tool(
            CallToolRequestParams::new(self.remote_name.clone()).with_arguments(arguments),
        );
        tokio::pin!(request);
        let result = tokio::select! {
            result = &mut request => result.map_err(|error| ToolError::Execution(error.to_string()))?,
            () = context.signal().wait() => return Err(ToolError::Aborted),
        };
        let details = serde_json::to_value(&result).ok();
        let is_error = result.is_error.unwrap_or(false);
        let content = result.content.into_iter().map(project_content).collect();
        Ok(ToolResult {
            content,
            details,
            usage: None,
            added_tool_names: None,
            is_error,
            terminate: false,
        })
    }
}

fn project_content(content: McpContentBlock) -> ContentBlock {
    match content {
        McpContentBlock::Text(text) => ContentBlock::Text(TextContent::new(text.text)),
        McpContentBlock::Image(image) => ContentBlock::Image(ImageContent {
            data: image.data,
            mime_type: image.mime_type,
        }),
        other => ContentBlock::Text(TextContent::new(
            serde_json::to_string(&other)
                .unwrap_or_else(|_| "[unsupported MCP content]".to_string()),
        )),
    }
}

fn qualified_tool_name(server: &str, tool: &str) -> String {
    format!(
        "mcp__{}__{}",
        identifier_component(server),
        identifier_component(tool)
    )
}

fn identifier_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            output.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator {
            output.push('_');
            previous_separator = true;
        }
    }
    output.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
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
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }
    }

    #[tokio::test]
    async fn discovers_and_invokes_tools_over_an_mcp_transport() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            let server = EchoServer::new().serve(server_io).await.unwrap();
            server.waiting().await.unwrap()
        });
        let client = ().serve(client_io).await.unwrap();
        let connected = connected_server("fixture server".to_string(), client);
        let remote = connected.peer.list_all_tools().await.unwrap().remove(0);
        let tool = McpTool::new(Arc::clone(&connected), remote);

        assert_eq!(tool.spec().name, "mcp__fixture_server__echo");
        let (_abort, signal) = pi_core::AbortHandle::new();
        let (updates, _receiver) = ToolUpdateSink::channel();
        let result = tool
            .execute(
                ToolContext::standalone(PathBuf::from("/tmp"), signal),
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
}
