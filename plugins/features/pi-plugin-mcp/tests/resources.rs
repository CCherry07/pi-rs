//! Resource behavior through the public MCP plugin and real local JSON-RPC requests.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::IntoResponse;
use pi_core::{AbortHandle, ContentBlock, ToolExecutionMode, ToolResult};
use pi_plugin::{FrozenRegistries, RegistriesBuilder, ToolContext, ToolError, ToolUpdateSink};
use pi_plugin_mcp::{McpLibrary, McpServerConfig, McpToolSet};
use serde_json::{Value, json};
use tokio::sync::Notify;

const RESOURCE_TOOLS: [&str; 3] = [
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
];

#[derive(Clone, Copy)]
struct Options {
    resources: bool,
    tools: bool,
    fail_list: bool,
    templates: bool,
    wait_read: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            resources: true,
            tools: false,
            fail_list: false,
            templates: true,
            wait_read: false,
        }
    }
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    read_started: Arc<Notify>,
    cancel_seen: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn new(options: Options) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let read_started = Arc::new(Notify::new());
        let cancel_seen = Arc::new(Notify::new());
        let cancel_read = Arc::new(Notify::new());
        let route = axum::routing::post({
            let requests = requests.clone();
            let read_started = read_started.clone();
            let cancel_seen = cancel_seen.clone();
            move |axum::Json(request): axum::Json<Value>| {
                let requests = requests.clone();
                let read_started = read_started.clone();
                let cancel_seen = cancel_seen.clone();
                let cancel_read = cancel_read.clone();
                async move {
                    requests.lock().unwrap().push(request.clone());
                    let method = request["method"].as_str().unwrap();
                    if method == "notifications/cancelled" {
                        cancel_read.notify_waiters();
                        cancel_seen.notify_one();
                    }
                    let id = &request["id"];
                    if id.is_null() {
                        return axum::http::StatusCode::ACCEPTED.into_response();
                    }
                    let error = |code, message| {
                        axum::Json(json!({
                            "jsonrpc":"2.0", "id":id,
                            "error":{"code":code,"message":message}
                        }))
                        .into_response()
                    };
                    let result = match method {
                        "server/discover" => return error(-32601, "Use initialize"),
                        "initialize" => {
                            let mut capabilities = json!({});
                            if options.resources {
                                capabilities["resources"] = json!({});
                            }
                            if options.tools {
                                capabilities["tools"] = json!({});
                            }
                            json!({
                                "protocolVersion":"2025-03-26", "capabilities":capabilities,
                                "serverInfo":{"name":"resources-fixture","version":"1"},
                                "instructions":"Use the fixture resource catalogue."
                            })
                        }
                        "tools/list" => json!({"tools":[{
                            "name":"echo", "inputSchema":{"type":"object"}
                        }]}),
                        "resources/list" if options.fail_list => {
                            return error(-32603, "listing unavailable");
                        }
                        "resources/list" if request["params"]["cursor"] == "next" => {
                            json!({"resources":[{"uri":"doc://second","name":"second"}]})
                        }
                        "resources/list" => json!({
                            "resources":[
                                {"uri":"doc://first","name":"first","_meta":{"secret":"metadata"},"icons":[{"src":"https://example.invalid/icon"}]},
                                {"uri":"ui://viewer","name":"viewer"},
                                {"uri":"doc://app","name":"app","mimeType":"text/html; profile=\"mcp-app\""}
                            ], "nextCursor":"next"
                        }),
                        "resources/templates/list" if !options.templates => {
                            return error(-32601, "templates unavailable");
                        }
                        "resources/templates/list" if request["params"]["cursor"] == "more" => {
                            json!({"resourceTemplates":[{"uriTemplate":"doc://archive/{id}","name":"archive"}]})
                        }
                        "resources/templates/list" => json!({
                            "resourceTemplates":[
                                {"uriTemplate":"doc://item/{id}","name":"item","_meta":{"secret":"metadata"}},
                                {"uriTemplate":"ui://viewer/{id}","name":"viewer"}
                            ], "nextCursor":"more"
                        }),
                        "resources/read" if options.wait_read => {
                            read_started.notify_one();
                            cancel_read.notified().await;
                            json!({"contents":[]})
                        }
                        "resources/read" => json!({
                            "contents":[
                                {"uri":"doc://text","text":"resource text","_meta":{"secret":"metadata"}},
                                {"uri":"doc://image","mimeType":"image/png","blob":"aW1hZ2U="},
                                {"uri":"doc://binary.bin","mimeType":"application/octet-stream","blob":"AAE="}
                            ], "_meta":{"secret":"response metadata"}
                        }),
                        other => panic!("unexpected method {other}"),
                    };
                    axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().route("/mcp", route))
                .await
                .unwrap();
        });
        Self {
            url,
            requests,
            read_started,
            cancel_seen,
            task,
        }
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == method)
            .cloned()
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn inspection_reports_hidden_tools_and_inherited_policy_without_listing_resources() {
    let server = Server::new(Options {
        tools: true,
        ..Options::default()
    })
    .await;
    let directory = tempfile::tempdir().unwrap();
    let agent = directory.path().join("agent");
    let project = directory.path().join("project");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(project.join(".pi")).unwrap();
    fs::write(
        agent.join("mcp.json"),
        json!({"mcpServers": {
            "fixture": {"url": server.url},
            "parked": {"command": "unused", "enabled": false},
            "invalid": {"args": ["missing command"]}
        }})
        .to_string(),
    )
    .unwrap();
    fs::write(
        project.join(".pi/mcp.json"),
        json!({"mcpServers": {
            "fixture": {"exposure": "direct", "toolExposure": {"echo": "hidden"}}
        }})
        .to_string(),
    )
    .unwrap();
    let library = McpLibrary::new(&agent, Some(&project), true);
    let report = library.inspect().await;
    assert!(!report.is_ok());
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.servers.len(), 2);
    let connected = &report.servers[0];
    assert_eq!(connected.state, pi_plugin_mcp::McpServerState::Connected);
    assert_eq!(connected.tools, ["echo"]);
    assert_eq!(connected.scope, pi_plugin_mcp::McpScope::Global);
    assert_eq!(connected.source, agent.join("mcp.json"));
    assert_eq!(
        connected.project_override,
        Some(project.join(".pi/mcp.json"))
    );
    assert_eq!(
        connected.tool_exposure.get("echo"),
        Some(&pi_plugin_mcp::McpExposure::Hidden)
    );
    assert!(connected.resources);
    assert_eq!(
        report.servers[1].state,
        pi_plugin_mcp::McpServerState::Disabled
    );
    assert_eq!(server.calls("initialize").len(), 1);
    assert_eq!(server.calls("tools/list").len(), 1);
    assert!(server.calls("resources/list").is_empty());
    assert!(server.calls("resources/templates/list").is_empty());
}

async fn execute(
    registry: &FrozenRegistries,
    name: &str,
    input: Value,
) -> Result<ToolResult, ToolError> {
    let tool = registry.tool(name).expect("resource tool registered");
    let (updates, _) = ToolUpdateSink::channel();
    tool.execute(
        ToolContext::standalone(std::env::temp_dir(), AbortHandle::new().1),
        "resource-call".into(),
        input,
        updates,
    )
    .await
}

fn payload(result: &ToolResult) -> Value {
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("expected JSON text")
    };
    serde_json::from_str(&text.text).unwrap()
}

async fn local_registry(servers: Value) -> (tempfile::TempDir, FrozenRegistries) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("mcp.json"),
        json!({"mcpServers":servers}).to_string(),
    )
    .unwrap();
    let plugin = McpLibrary::new(directory.path(), None, false)
        .prepare()
        .await
        .unwrap();
    let (_, registry) = RegistriesBuilder::new()
        .register_plugins(vec![plugin])
        .unwrap();
    (directory, registry)
}

#[tokio::test]
async fn resources_only_server_exposes_paged_tools_and_projects_read_contents() {
    let server = Server::new(Options::default()).await;
    let pool = McpToolSet::connect(vec![McpServerConfig::http("docs", &server.url)])
        .await
        .unwrap();
    let (_, registry) = RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    for name in RESOURCE_TOOLS {
        assert_eq!(
            registry.tool(name).unwrap().spec().execution_mode,
            ToolExecutionMode::Parallel
        );
    }
    assert!(server.calls("tools/list").is_empty());
    let first = execute(&registry, "list_mcp_resources", json!({"server":" docs "}))
        .await
        .unwrap();
    assert_eq!(
        payload(&first),
        json!({"server":"docs","resources":[{"server":"docs","uri":"doc://first","name":"first"}],"nextCursor":"next"})
    );
    assert_eq!(first.details, Some(payload(&first)));
    let second = execute(
        &registry,
        "list_mcp_resources",
        json!({"server":"docs","cursor":" next "}),
    )
    .await
    .unwrap();
    assert_eq!(payload(&second)["resources"][0]["uri"], "doc://second");
    let calls = server.calls("resources/list").len();
    assert!(
        execute(&registry, "list_mcp_resources", json!({"cursor":"next"}))
            .await
            .is_err()
    );
    assert_eq!(server.calls("resources/list").len(), calls);

    let templates = execute(&registry, "list_mcp_resource_templates", json!({}))
        .await
        .unwrap();
    assert_eq!(
        payload(&templates)["resourceTemplates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        payload(&templates)["resourceTemplates"][1]["uriTemplate"],
        "doc://archive/{id}"
    );
    assert!(
        payload(&templates)["resourceTemplates"][0]
            .get("_meta")
            .is_none()
    );

    // Pi filters UI resources from listings but leaves explicit read URIs to the server.
    let result = execute(
        &registry,
        "read_mcp_resource",
        json!({"server":"docs","uri":"ui://explicit"}),
    )
    .await
    .unwrap();
    assert_eq!(
        server.calls("resources/read")[0]["params"]["uri"],
        "ui://explicit"
    );
    assert!(
        result
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text(text) if text.text == "resource text"))
    );
    assert!(result.content.iter().any(|block| matches!(block, ContentBlock::Image(image) if image.data == "aW1hZ2U=" && image.mime_type == "image/png")));
    let saved = result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(text) => text
                .text
                .split_once(" saved to ")
                .map(|(_, path)| path.trim_end_matches(']')),
            _ => None,
        })
        .expect("binary file path");
    assert_eq!(fs::read(saved).unwrap(), [0, 1]);
    fs::remove_file(saved).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["server"], "docs");
    assert_eq!(details["uri"], "ui://explicit");
    assert!(details["contents"][0].get("_meta").is_none());
    pool.shutdown().await.unwrap();
}

#[tokio::test]
async fn aggregation_preserves_available_pages_and_isolates_failed_and_hidden_servers() {
    let visible = Server::new(Options {
        templates: false,
        ..Default::default()
    })
    .await;
    let failed = Server::new(Options {
        fail_list: true,
        ..Default::default()
    })
    .await;
    let hidden = Server::new(Options {
        tools: true,
        ..Default::default()
    })
    .await;
    let (_directory, registry) = local_registry(json!({
        "alpha":{"url":visible.url},
        "zeta":{"url":failed.url},
        "private":{"url":hidden.url,"exposure":"hidden","toolExposure":{"echo":"direct"}}
    }))
    .await;
    assert!(registry.tool("mcp__private__echo").is_some());
    let result = execute(&registry, "list_mcp_resources", json!({}))
        .await
        .unwrap();
    let result = payload(&result);
    assert_eq!(result["resources"].as_array().unwrap().len(), 2);
    assert_eq!(result["resources"][0]["server"], "alpha");
    assert_eq!(result["resources"][1]["uri"], "doc://second");
    assert_eq!(result["errors"][0]["server"], "zeta");
    assert!(hidden.calls("resources/list").is_empty());
    assert!(
        execute(
            &registry,
            "read_mcp_resource",
            json!({"server":"private","uri":"doc://first"})
        )
        .await
        .is_err()
    );
    assert!(hidden.calls("resources/read").is_empty());
    let templates = execute(
        &registry,
        "list_mcp_resource_templates",
        json!({"server":"alpha"}),
    )
    .await
    .unwrap();
    assert_eq!(
        payload(&templates),
        json!({"server":"alpha","resourceTemplates":[]})
    );
}

#[tokio::test]
async fn no_resource_capability_or_only_hidden_resources_registers_no_resource_tools() {
    for (options, exposure) in [
        (
            Options {
                resources: false,
                ..Default::default()
            },
            "direct",
        ),
        (Options::default(), "hidden"),
    ] {
        let server = Server::new(options).await;
        let (_directory, registry) =
            local_registry(json!({"docs":{"url":server.url,"exposure":exposure}})).await;
        for name in RESOURCE_TOOLS {
            assert!(registry.tool(name).is_none());
        }
        assert!(server.calls("resources/list").is_empty());
    }
}

#[tokio::test]
async fn deferred_resources_use_the_widest_visible_server_exposure() {
    let deferred = Server::new(Options::default()).await;
    let direct = Server::new(Options::default()).await;
    for direct_exposure in ["hidden", "direct"] {
        let (_directory, registry) = local_registry(json!({
            "docs":{"url":deferred.url,"exposure":"deferred"},
            "other":{"url":direct.url,"exposure":direct_exposure}
        }))
        .await;
        let defaults = registry
            .default_tool_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert!(defaults.iter().any(|name| name == "tool_search"));
        for name in RESOURCE_TOOLS {
            assert!(registry.tool(name).is_some());
            assert_eq!(
                defaults.iter().any(|tool| tool == name),
                direct_exposure == "direct"
            );
        }
        let candidates = registry.discoverable_tools(&defaults);
        assert_eq!(
            candidates.len(),
            if direct_exposure == "direct" { 0 } else { 3 }
        );
    }
}

#[tokio::test]
async fn deferred_config_activates_discovery_even_when_no_tool_matches() {
    let server = Server::new(Options {
        resources: false,
        ..Default::default()
    })
    .await;
    let (_directory, registry) = local_registry(json!({
        "docs":{"url":server.url,"exposure":"hidden","toolExposure":{"not-registered":"deferred"}}
    }))
    .await;
    let defaults = registry
        .default_tool_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert_eq!(defaults, ["tool_search"]);
    assert!(registry.discoverable_tools(&defaults).is_empty());
}

#[tokio::test]
async fn server_directory_uses_configured_exposure_without_active_entry_points_or_extra_summaries()
{
    let server = Server::new(Options::default()).await;
    let directory = tempfile::tempdir().unwrap();
    for (exposure, reach, instruction) in [
        (
            "codemode",
            "codemode",
            "Call the tools of `codemode` servers from codemode scripts.",
        ),
        (
            "deferred",
            "tool_search",
            "Load the tools of `tool_search` servers with `tool_search`.",
        ),
        ("direct", "", ""),
    ] {
        fs::write(
            directory.path().join("mcp.json"),
            json!({"mcpServers":{
                "docs":{"url":server.url,"exposure":exposure,"description":"Reference library"}
            }})
            .to_string(),
        )
        .unwrap();
        let plugin = McpLibrary::new(directory.path(), None, false)
            .prepare()
            .await
            .unwrap();
        let (driver, registry) = RegistriesBuilder::new()
            .register_plugins(vec![plugin])
            .unwrap();
        assert!(registry.tool_infos().iter().any(|tool| {
            tool.discovery.namespace.as_ref().is_some_and(|namespace| {
                namespace
                    .instructions
                    .as_deref()
                    .is_some_and(|instructions| {
                        instructions.contains("Use the fixture resource catalogue.")
                    })
            })
        }));
        for active_tools in [Vec::new(), vec!["read_mcp_resource".to_string()]] {
            let patch = driver
                .before_agent_start(
                    &pi_core::RunId::next(),
                    directory.path(),
                    &AbortHandle::new().1,
                    pi_plugin::BeforeAgentStartEvent {
                        system_prompt: "base".into(),
                        prompt_state: None,
                        input_messages: Vec::new(),
                        active_tools,
                        provider_id: "scripted".into(),
                        model_id: "test".into(),
                    },
                )
                .await
                .unwrap();
            let expected = if exposure == "direct" {
                "base".to_string()
            } else {
                format!(
                    "base\n\n<mcp_servers>\nMCP servers whose tools are not declared to you. {instruction}\n- mcp__docs ({reach}): Reference library\n</mcp_servers>"
                )
            };
            assert_eq!(patch.system_prompt.as_deref(), Some(expected.as_str()));
        }
    }
}

#[tokio::test]
async fn host_supplied_tool_search_does_not_duplicate_the_standalone_helper() {
    let server = Server::new(Options::default()).await;
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("mcp.json"),
        json!({"mcpServers":{
            "docs":{"url":server.url,"exposure":"deferred"}
        }})
        .to_string(),
    )
    .unwrap();
    let library =
        McpLibrary::new(directory.path(), None, false).with_tool_search_registration(false);
    let plugin = library.prepare().await.unwrap();
    let (_, mcp_only) = RegistriesBuilder::new()
        .register_plugins(vec![plugin.clone()])
        .unwrap();
    assert!(mcp_only.tool("tool_search").is_none());
    for host_first in [false, true] {
        let search: Arc<dyn pi_plugin::Plugin> = Arc::new(pi_plugin_tool_search::ToolSearchPlugin);
        let plugins = if host_first {
            vec![search, plugin.clone()]
        } else {
            vec![plugin.clone(), search]
        };
        let (_, registry) = RegistriesBuilder::new().register_plugins(plugins).unwrap();
        assert_eq!(
            registry
                .tool_infos()
                .iter()
                .filter(|tool| tool.spec.name == "tool_search")
                .count(),
            1
        );
        assert!(registry.tool("tool_search").unwrap().reaches_tools());
        for name in RESOURCE_TOOLS {
            assert!(registry.tool(name).is_some());
        }
    }
}

#[tokio::test]
async fn resource_timeout_and_abort_cancel_remote_reads() {
    for abort_call in [false, true] {
        let server = Server::new(Options {
            wait_read: true,
            ..Default::default()
        })
        .await;
        let (_directory, registry) = local_registry(
            json!({"docs":{"url":server.url,"timeout":if abort_call { 5.0 } else { 0.15 }}}),
        )
        .await;
        let tool = registry.tool("read_mcp_resource").unwrap();
        let (abort, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let call = tokio::spawn(async move {
            tool.execute(
                ToolContext::standalone(std::env::temp_dir(), signal),
                "read".into(),
                json!({"server":"docs","uri":"doc://wait"}),
                updates,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), server.read_started.notified())
            .await
            .unwrap();
        if abort_call {
            abort.abort();
        }
        let result = tokio::time::timeout(Duration::from_secs(3), call)
            .await
            .unwrap()
            .unwrap();
        if abort_call {
            assert!(matches!(result, Err(ToolError::Aborted)));
        } else {
            assert!(
                matches!(result, Err(ToolError::Execution(error)) if error.contains("timed out"))
            );
        }
        tokio::time::timeout(Duration::from_secs(3), server.cancel_seen.notified())
            .await
            .unwrap();
    }
}
