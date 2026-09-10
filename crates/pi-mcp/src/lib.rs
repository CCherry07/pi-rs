#![forbid(unsafe_code)]

//! Protocol-neutral MCP client integration for Pi runtime generations.
//!
//! This crate owns MCP transports, discovery, invocation, and cleanup. It has
//! no knowledge of ACP or Pi session persistence; callers adapt their wire
//! configuration into [`McpServerConfig`] and inject [`McpToolSet::plugin`]
//! through their own generation seam.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, ContentBlock, ImageContent, PluginId, RegisterContext, TextContent, Tool,
    ToolCallId, ToolContext, ToolError, ToolExecutionMode, ToolResult, ToolSpec, ToolUpdateSink,
};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientRequest, ContentBlock as McpContentBlock,
    ServerResult, Tool as McpToolSpec,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{Peer, RoleClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
}

impl McpServerConfig {
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransport::Http {
                url: url.into(),
                headers: BTreeMap::new(),
            },
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
            let name = config.name.clone();
            let server = tokio::time::timeout(CONNECT_TIMEOUT, connect_server(config))
                .await
                .map_err(|_| McpError::Timeout(name.clone()))??;
            let remote_tools = tokio::time::timeout(CONNECT_TIMEOUT, server.peer.list_all_tools())
                .await
                .map_err(|_| McpError::Timeout(name))?
                .map_err(|_| McpError::ListTools {
                    server: server.name.clone(),
                    message: "server rejected discovery or the connection closed".into(),
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

async fn connect_server(config: McpServerConfig) -> Result<Arc<ConnectedServer>, McpError> {
    let McpServerConfig { name, transport } = config;
    let auto = ClientLifecycleMode::Auto {
        preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
        legacy_version: Some(rmcp::model::ProtocolVersion::LATEST),
    };
    let service = match connect_once(&name, &transport, auto).await {
        Ok(service) => service,
        // Some deployed legacy servers return an uncorrelated JSON-RPC error
        // to server/discover. A fresh transport can still complete the stable
        // initialize handshake; retrying cannot reuse ambiguous transport state.
        Err(McpError::Initialize { .. }) => {
            connect_once(&name, &transport, ClientLifecycleMode::Initialize).await?
        }
        Err(error) => return Err(error),
    };
    Ok(connected_server(name, service))
}

async fn connect_once(
    name: &str,
    config: &McpTransport,
    lifecycle: ClientLifecycleMode,
) -> Result<RunningService<RoleClient, ()>, McpError> {
    match config {
        McpTransport::Http { url, headers } => {
            // reqwest 0.13's no-provider feature requires an explicit default.
            // Respect an embedding host's choice, otherwise match Pi's ring backend.
            if rustls::crypto::CryptoProvider::get_default().is_none() {
                let _ = rustls::crypto::ring::default_provider().install_default();
            }
            let headers = http_headers(name, headers)?;
            let transport = StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(url.clone()).custom_headers(headers),
            );
            ().serve_with_lifecycle(transport, lifecycle).await
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
            ().serve_with_lifecycle(transport, lifecycle).await
        }
    }
    .map_err(|_| McpError::Initialize {
        server: name.to_string(),
        message: "handshake failed; check endpoint, credentials and server availability".into(),
    })
}

fn connected_server(name: String, service: RunningService<RoleClient, ()>) -> Arc<ConnectedServer> {
    Arc::new(ConnectedServer {
        name,
        peer: service.peer().clone(),
        service: Mutex::new(Some(service)),
    })
}

pub fn validate_configs(configs: &[McpServerConfig]) -> Result<(), McpError> {
    let mut names = HashSet::new();
    for config in configs {
        let name = config.name.trim();
        if name.is_empty() {
            return Err(McpError::EmptyServerName);
        }
        if !names.insert(name.to_string()) {
            return Err(McpError::DuplicateServerName(name.to_string()));
        }
        match &config.transport {
            McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                return Err(McpError::EmptyCommand(name.to_string()));
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
            }
            _ => {}
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
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let failure = || {
            ToolError::Execution(format!(
                "MCP request failed for server {}",
                self.server.name
            ))
        };
        let mut request = self
            .server
            .peer
            .send_cancellable_request(
                ClientRequest::CallToolRequest(CallToolRequest::new(
                    CallToolRequestParams::new(self.remote_name.clone()).with_arguments(arguments),
                )),
                Default::default(),
            )
            .await
            .map_err(|_| failure())?;
        let response = tokio::select! {
            result = &mut request.rx => result.map_err(|_| failure())?.map_err(|_| failure())?,
            () = context.signal().wait() => {
                // The SDK sends a legacy cancellation notification or closes the
                // modern request stream, depending on the negotiated lifecycle.
                let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, request.cancel(Some("cancelled by user".into()))).await;
                return Err(ToolError::Aborted);
            },
        };
        let ServerResult::CallToolResult(result) = response else {
            return Err(failure());
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
        let server = connect_server(config).await.unwrap();
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
