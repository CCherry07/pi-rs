//! Deferred MCP tools use the ordinary provider, tool-selection and session tree paths.
//!
//! Oracle: legacy/pi/packages/coding-agent/test/suite/agent-session-mcp.test.ts,
//! "activates tool_search for deferred MCP tools and keeps loaded tools declared on the branch".

use std::collections::VecDeque;
use std::fs;
use std::sync::{Arc, Mutex};

use axum::response::IntoResponse;
use pi_coding::{Config, Pi};
use pi_core::{ContentBlock, Message};
use pi_session::{PiSession, PromptOutcome, SessionEntry};
use serde_json::{Value, json};

const SEARCH: &str = "mcp__docs__search";
const UNUSED: &str = "mcp__docs__distance";
const HIDDEN: &str = "mcp__docs__secret";
const RESOURCE_TOOLS: [&str; 3] = [
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
];

#[derive(Default)]
struct ProviderScript {
    calls: VecDeque<(&'static str, Value)>,
    requests: Vec<Value>,
    next_call_id: usize,
}

async fn provider_response(
    axum::extract::State(script): axum::extract::State<Arc<Mutex<ProviderScript>>>,
    axum::Json(request): axum::Json<Value>,
) -> impl IntoResponse {
    let (delta, finish_reason) = {
        let mut script = script.lock().unwrap();
        script.requests.push(request);
        if let Some((name, arguments)) = script.calls.pop_front() {
            script.next_call_id += 1;
            (
                json!({"role":"assistant","tool_calls":[{
                    "index":0,"id":format!("deferred-{}", script.next_call_id),"type":"function",
                    "function":{"name":name,"arguments":arguments.to_string()}
                }]}),
                "tool_calls",
            )
        } else {
            (
                json!({"role":"assistant","content":"Fixture complete."}),
                "stop",
            )
        }
    };
    let chunk = json!({
        "id":"deferred","object":"chat.completion.chunk","model":"test",
        "choices":[{"index":0,"delta":delta,"finish_reason":null}]
    });
    let end = json!({
        "id":"deferred","object":"chat.completion.chunk","model":"test",
        "choices":[{"index":0,"delta":{},"finish_reason":finish_reason}]
    });
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
    )
}

struct Fixture {
    directory: tempfile::TempDir,
    pi: Pi,
    session: PiSession,
    base: String,
    script: Arc<Mutex<ProviderScript>>,
    remote_calls: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(resource_only: bool, policy: Value) -> Self {
        Self::configured(resource_only, policy, None, None).await
    }

    async fn configured(
        resource_only: bool,
        policy: Value,
        auto_enable: Option<bool>,
        default_tools: Option<&[&str]>,
    ) -> Self {
        let remote_calls = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(ProviderScript::default()));
        let router = axum::Router::new()
            .route(
                "/mcp",
                axum::routing::post({
                    let remote_calls = Arc::clone(&remote_calls);
                    move |axum::Json(request): axum::Json<Value>| {
                        let remote_calls = Arc::clone(&remote_calls);
                        async move {
                            let id = &request["id"];
                            if id.is_null() {
                                return axum::http::StatusCode::ACCEPTED.into_response();
                            }
                            let result = match request["method"].as_str().unwrap() {
                                "server/discover" => {
                                    return axum::Json(json!({
                                        "jsonrpc":"2.0","id":id,
                                        "error":{"code":-32601,"message":"Use initialize"}
                                    }))
                                    .into_response();
                                }
                                "initialize" => json!({
                                    "protocolVersion":"2025-03-26",
                                    "capabilities":if resource_only {json!({"resources":{}})} else {json!({"tools":{}})},
                                    "serverInfo":{"name":"deferred-fixture","version":"1"},
                                    "instructions":"Reference collection guidance."
                                }),
                                "tools/list" => json!({"tools":[
                                    {"name":"search","description":"Search the docs.","inputSchema":{
                                        "type":"object","properties":{"query":{"type":"string"}},"required":["query"]
                                    }},
                                    {"name":"distance","description":"Calculate distance.","inputSchema":{"type":"object"}},
                                    {"name":"secret","description":"Private secret.","inputSchema":{"type":"object"}}
                                ]}),
                                "tools/call" => {
                                    remote_calls.lock().unwrap().push(request.clone());
                                    json!({"content":[{"type":"text","text":format!("{} guide",request["params"]["arguments"]["query"].as_str().unwrap())}]})
                                }
                                "resources/list" => {
                                    remote_calls.lock().unwrap().push(request.clone());
                                    json!({"resources":[{"uri":"doc://guide","name":"guide"}]})
                                }
                                "resources/templates/list" => {
                                    remote_calls.lock().unwrap().push(request.clone());
                                    json!({"resourceTemplates":[{"uriTemplate":"doc://guide/{id}","name":"guide"}]})
                                }
                                "resources/read" => {
                                    remote_calls.lock().unwrap().push(request.clone());
                                    json!({"contents":[{"uri":"doc://guide","text":"Deferred resource contents."}]})
                                }
                                method => panic!("unexpected MCP request {method}"),
                            };
                            axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result}))
                                .into_response()
                        }
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                axum::routing::post(provider_response),
            )
            .with_state(Arc::clone(&script));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let project = directory.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            agent.join("memory.json"),
            r#"{"version":1,"enabled":false}"#,
        )
        .unwrap();
        Self::write_policy(&agent.join("mcp.json"), &base, policy);
        if let Some(value) = auto_enable {
            Self::write_auto_enable(&agent.join("mcp.json"), json!(value));
        }
        if let Some(tools) = default_tools {
            fs::write(
                agent.join("settings.json"),
                json!({"defaultTools":tools}).to_string(),
            )
            .unwrap();
        }
        fs::write(
            agent.join("models.json"),
            json!({"providers":{"fixture":{
                "baseUrl":format!("{base}/v1"),"api":"openai-completions","apiKey":"fixture",
                "models":[{"id":"test","contextWindow":100000,"maxTokens":1024}]
            }}})
            .to_string(),
        )
        .unwrap();
        let mut config = Config::new(project.clone(), agent.clone());
        config.provider = "fixture".into();
        config.model = Some("test".into());
        config.discover_extensions = false;
        config.trust_override = Some(false);
        let pi = Pi::builder(config).build().unwrap();
        let session = pi
            .sessions()
            .create_session(pi_session::CreateSessionOptions::new(
                &project,
                agent.join("session.jsonl"),
            ))
            .await
            .unwrap();
        Self {
            directory,
            pi,
            session,
            base,
            script,
            remote_calls,
            server,
        }
    }

    fn write_policy(path: &std::path::Path, base: &str, policy: Value) {
        let mut entry = json!({"url":format!("{base}/mcp"),"description":"Reference collection"});
        entry
            .as_object_mut()
            .unwrap()
            .extend(policy.as_object().unwrap().clone());
        fs::write(path, json!({"mcpServers":{"docs":entry}}).to_string()).unwrap();
    }

    fn set_policy(&self, policy: Value) {
        Self::write_policy(
            &self.directory.path().join("agent/mcp.json"),
            &self.base,
            policy,
        );
    }

    fn write_auto_enable(path: &std::path::Path, value: Value) {
        let mut document: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        document["autoEnableCodemode"] = value;
        fs::write(path, document.to_string()).unwrap();
    }

    fn set_auto_enable(&self, value: Value) {
        Self::write_auto_enable(&self.directory.path().join("agent/mcp.json"), value);
    }

    fn set_script(&self, calls: impl IntoIterator<Item = (&'static str, Value)>) {
        let mut script = self.script.lock().unwrap();
        script.calls = calls.into_iter().collect();
        script.requests.clear();
    }

    fn assert_active(&self, name: &str, expected: bool) {
        assert_eq!(
            self.session
                .current()
                .snapshot()
                .agent
                .active_tools
                .iter()
                .any(|active| active == name),
            expected,
            "active tool {name}"
        );
    }

    async fn shutdown(self) {
        self.pi.sessions().shutdown().await.unwrap();
        self.server.abort();
    }
}

fn declares(request: &Value, name: &str) -> bool {
    request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|tool| tool["function"]["name"] == name)
}

fn system_prompt(request: &Value) -> String {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| matches!(message["role"].as_str(), Some("system" | "developer")))
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_initial_discovery_prompt(request: &Value) {
    let prompt = system_prompt(request);
    let (_, section) = prompt.split_once("<mcp_servers>\n").unwrap();
    let (section, _) = section.split_once("\n</mcp_servers>").unwrap();
    assert!(section.chars().count() <= 4096);
    assert!(section.contains("Reference collection"));
    assert!(section.contains("tool_search"));
    assert!(!prompt.contains("Reference collection guidance."));
}

fn assert_loaded_instructions(request: &Value) {
    assert_eq!(
        system_prompt(request)
            .matches("Reference collection guidance.")
            .count(),
        1,
        "active namespace instructions occur once in each provider request"
    );
}

#[tokio::test]
async fn deferred_mcp_search_loads_tools_and_preserves_selection_across_tree_reload_and_resume() {
    let mut fixture = Fixture::new(
        false,
        json!({"exposure":"deferred","toolExposure":{"secret":"hidden"}}),
    )
    .await;
    fixture.assert_active("tool_search", true);
    fixture.assert_active(SEARCH, false);
    fixture.assert_active(UNUSED, false);
    assert!(
        !fixture
            .session
            .current()
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == HIDDEN)
    );
    fixture.set_script([
        ("tool_search", json!({"query":"search the docs","limit":1})),
        (SEARCH, json!({"query":"loaded"})),
        ("tool_search", json!({"query":"search"})),
        ("tool_search", json!({"query":"secret"})),
    ]);
    let PromptOutcome::Agent(outcome) = fixture
        .session
        .current()
        .prompt("Find and use the docs tool.")
        .await
        .unwrap()
    else {
        panic!("expected agent run");
    };
    let searches = outcome
        .new_messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "tool_search" => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(searches.len(), 3);
    assert_eq!(searches[0].details, Some(json!({"loaded":[SEARCH]})));
    assert_eq!(searches[0].added_tool_names, Some(vec![SEARCH.to_string()]));
    for result in &searches[1..] {
        assert!(!result.is_error);
        assert_eq!(result.details, Some(json!({"loaded":[]})));
        assert!(result.content.iter().any(|content| matches!(content,
            ContentBlock::Text(text) if text.text == "No matching tools found."
        )));
    }
    assert!(outcome.new_messages.iter().any(|message| matches!(message,
        Message::ToolResult(result) if result.tool_name == SEARCH && !result.is_error
            && result.content.iter().any(|content| matches!(content, ContentBlock::Text(text) if text.text == "loaded guide"))
    )));
    fixture.assert_active(SEARCH, true);
    fixture.assert_active(UNUSED, false);
    let budget = fixture.session.current().context_budget().unwrap();
    let mcp = budget
        .request
        .categories
        .iter()
        .find(|part| part.category == pi_core::ContextCategory::McpTools)
        .unwrap();
    assert!(
        mcp.items
            .iter()
            .any(|item| item.name == SEARCH && item.tokens > 0)
    );
    assert!(
        mcp.items
            .iter()
            .any(|item| item.name.ends_with(" instructions"))
    );
    assert!(
        !budget
            .request
            .undeclared_mcp_tools
            .contains(&SEARCH.to_string())
    );
    assert!(
        budget
            .request
            .undeclared_mcp_tools
            .contains(&UNUSED.to_string())
    );
    assert!(
        !budget
            .request
            .undeclared_mcp_tools
            .contains(&HIDDEN.to_string())
    );
    {
        let script = fixture.script.lock().unwrap();
        assert_eq!(script.requests.len(), 5);
        assert!(declares(&script.requests[0], "tool_search"));
        assert!(!declares(&script.requests[0], SEARCH));
        assert!(declares(&script.requests[1], SEARCH));
        assert_initial_discovery_prompt(&script.requests[0]);
        for request in &script.requests[1..] {
            assert_loaded_instructions(request);
        }
        assert!(
            script
                .requests
                .iter()
                .all(|request| { !declares(request, UNUSED) && !declares(request, HIDDEN) })
        );
    }
    assert_eq!(fixture.remote_calls.lock().unwrap().len(), 1);

    let current = fixture.session.current();
    let session_path = current.history().path().to_path_buf();
    let last = current.history().leaf_id().unwrap();
    let first_user = current
        .history()
        .branch_entries()
        .unwrap()
        .into_iter()
        .find(|record| matches!(&record.entry,
            SessionEntry::Message(message) if matches!(message.message.as_standard(), Some(Message::User(_)))
        ))
        .unwrap()
        .id;
    current.checkout(Some(&first_user)).await.unwrap();
    fixture.assert_active(SEARCH, false);
    current.checkout(Some(&last)).await.unwrap();
    fixture.assert_active(SEARCH, true);
    drop(current);

    fixture.session.reload().await.unwrap();
    fixture.assert_active(SEARCH, true);
    fixture.assert_active(UNUSED, false);
    fixture
        .pi
        .sessions()
        .close_session(&fixture.session)
        .await
        .unwrap();
    fixture.session = fixture
        .pi
        .sessions()
        .open_session(&session_path, Default::default())
        .await
        .unwrap();
    fixture.assert_active(SEARCH, true);
    fixture.assert_active(UNUSED, false);
    fixture.set_script([(SEARCH, json!({"query":"again"}))]);
    fixture
        .session
        .current()
        .prompt("Use the loaded tool after resume.")
        .await
        .unwrap();
    {
        let script = fixture.script.lock().unwrap();
        assert!(declares(&script.requests[0], SEARCH));
        assert_loaded_instructions(&script.requests[0]);
    }
    assert_eq!(fixture.remote_calls.lock().unwrap().len(), 2);

    fixture.set_policy(json!({"exposure":"hidden"}));
    fixture.session.reload().await.unwrap();
    fixture.assert_active(SEARCH, false);
    assert!(
        !fixture
            .session
            .current()
            .catalog()
            .tool_specs()
            .iter()
            .any(|tool| tool.name == SEARCH)
    );
    fixture.set_script([]);
    fixture
        .session
        .current()
        .prompt("Continue after hiding the server.")
        .await
        .unwrap();
    {
        let script = fixture.script.lock().unwrap();
        assert!(!declares(&script.requests[0], SEARCH));
        assert!(!system_prompt(&script.requests[0]).contains("Reference collection guidance."));
    }
    assert_eq!(fixture.remote_calls.lock().unwrap().len(), 2);
    assert!(
        fs::read_to_string(&session_path)
            .unwrap()
            .contains("loaded guide")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn deferred_resource_only_servers_load_shared_tools_on_demand() {
    let fixture = Fixture::new(true, json!({"exposure":"deferred"})).await;
    fixture.assert_active("tool_search", true);
    for name in RESOURCE_TOOLS {
        fixture.assert_active(name, false);
    }
    fixture.set_script([
        (
            "tool_search",
            json!({"query":"list_mcp_resources","limit":1}),
        ),
        ("list_mcp_resources", json!({"server":"docs"})),
        (
            "tool_search",
            json!({"query":"read_mcp_resource","limit":1}),
        ),
        (
            "read_mcp_resource",
            json!({"server":"docs","uri":"doc://guide"}),
        ),
    ]);
    let PromptOutcome::Agent(outcome) = fixture
        .session
        .current()
        .prompt("Find and read the guide resource.")
        .await
        .unwrap()
    else {
        panic!("expected agent run");
    };
    assert!(outcome.new_messages.iter().any(|message| matches!(message,
        Message::ToolResult(result) if result.tool_name == "read_mcp_resource" && !result.is_error
            && result.content.iter().any(|content| matches!(content, ContentBlock::Text(text) if text.text == "Deferred resource contents."))
    )));
    {
        let script = fixture.script.lock().unwrap();
        assert_eq!(script.requests.len(), 5);
        assert!(
            RESOURCE_TOOLS
                .iter()
                .all(|name| !declares(&script.requests[0], name))
        );
        assert!(declares(&script.requests[1], "list_mcp_resources"));
        assert!(!declares(&script.requests[1], "read_mcp_resource"));
        assert!(declares(&script.requests[3], "read_mcp_resource"));
        assert_initial_discovery_prompt(&script.requests[0]);
        for request in &script.requests[1..] {
            assert_loaded_instructions(request);
        }
    }
    {
        let calls = fixture.remote_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["method"], "resources/list");
        assert_eq!(calls[1]["method"], "resources/read");
        assert_eq!(calls[1]["params"]["uri"], "doc://guide");
    }
    fixture.assert_active("list_mcp_resources", true);
    fixture.assert_active("read_mcp_resource", true);
    fixture.assert_active("list_mcp_resource_templates", false);
    fixture.shutdown().await;
}

#[tokio::test]
async fn deferred_override_activates_search_even_before_any_matching_tool_exists() {
    let fixture = Fixture::new(
        false,
        json!({"exposure":"direct","toolExposure":{"not_registered_yet":"deferred","secret":"hidden"}}),
    )
    .await;
    fixture.assert_active("tool_search", true);
    fixture.assert_active(SEARCH, true);
    fixture.assert_active(UNUSED, true);
    fixture.assert_active(HIDDEN, false);
    fixture.shutdown().await;
}

// Oracle: MCP ensureDiscoveryActive and session_start in legacy/pi's extensions/mcp/index.ts.
#[tokio::test]
async fn auto_codemode_appends_before_first_request_and_executes_mcp_without_declaring_it() {
    let fixture = Fixture::configured(false, json!({}), None, Some(&["read"])).await;
    let current = fixture.session.current();
    assert_eq!(current.snapshot().agent.active_tools, ["read", "codemode"]);
    assert!(
        !current.history().path().exists(),
        "startup selection must remain lazily persisted"
    );
    fixture.set_script([(
        "codemode",
        json!({"code":
            "text(await tools.mcp__docs__search({query: 'automatic'}));"
        }),
    )]);
    let PromptOutcome::Agent(outcome) = current
        .prompt("Search the docs through codemode.")
        .await
        .unwrap()
    else {
        panic!("expected agent run");
    };
    assert!(outcome.new_messages.iter().any(|message| matches!(message,
        Message::ToolResult(result) if result.tool_name == "codemode" && !result.is_error
            && result.content.iter().any(|content| matches!(content, ContentBlock::Text(text) if text.text.contains("automatic guide")))
    )));
    assert!(current.history().path().exists());
    assert_eq!(fixture.remote_calls.lock().unwrap().len(), 1);
    let budget = current.context_budget().unwrap();
    assert!(
        budget
            .request
            .undeclared_mcp_tools
            .contains(&SEARCH.to_string())
    );
    assert!(
        budget
            .request
            .undeclared_mcp_tools
            .contains(&UNUSED.to_string())
    );
    let mcp = budget
        .request
        .categories
        .iter()
        .find(|part| part.category == pi_core::ContextCategory::McpTools)
        .unwrap();
    assert!(
        mcp.items
            .iter()
            .all(|item| item.name != SEARCH && item.name != UNUSED)
    );
    assert!(
        mcp.items
            .iter()
            .any(|item| item.name == "MCP server directory")
    );
    let tools = budget
        .request
        .categories
        .iter()
        .find(|part| part.category == pi_core::ContextCategory::SystemTools)
        .unwrap();
    assert!(tools.items.iter().any(|item| item.name == "codemode"));
    {
        let script = fixture.script.lock().unwrap();
        assert!(declares(&script.requests[0], "codemode"));
        assert!(
            script
                .requests
                .iter()
                .all(|request| !declares(request, SEARCH))
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn codemode_resource_declarations_match_their_structured_returns() {
    let fixture = Fixture::configured(true, json!({}), None, Some(&["read"])).await;
    fixture.set_script([("codemode", json!({"code":r#"
        const cases = [
            ['list_mcp_resources', {}, 'resources', 'uri'],
            ['list_mcp_resource_templates', {}, 'resourceTemplates', 'uriTemplate'],
            ['read_mcp_resource', {server: 'docs', uri: 'doc://guide'}, 'contents', 'text']
        ];
        for (const [name, args, field, itemField] of cases) {
            const declaration = await describeTool(name);
            if (!declaration.includes('Promise<{') || !declaration.includes(field + ':') || !declaration.includes(itemField + ':')) {
                throw new Error('Incorrect resource return declaration: ' + declaration);
            }
            const value = await tools[name](args);
            if (!Array.isArray(value[field]) || value[field].length !== 1 || typeof value[field][0][itemField] !== 'string') {
                throw new Error('Resource return does not match its declaration: ' + JSON.stringify(value));
            }
            text({name, value});
        }
    "#}))]);
    let PromptOutcome::Agent(outcome) = fixture
        .session
        .current()
        .prompt("Inspect the resource tools and use them.")
        .await
        .unwrap()
    else {
        panic!("expected agent run");
    };
    let result = outcome
        .new_messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_name == "codemode" => Some(result),
            _ => None,
        })
        .expect("codemode executed");
    assert!(!result.is_error, "{:?}", result.content);
    let content = serde_json::to_string(&result.content).unwrap();
    for expected in [
        "resources",
        "resourceTemplates",
        "contents",
        "Deferred resource contents.",
    ] {
        assert!(content.contains(expected), "missing {expected}: {content}");
    }
    assert_eq!(fixture.remote_calls.lock().unwrap().len(), 3);
    fixture.assert_active("tool_search", false);
    {
        let script = fixture.script.lock().unwrap();
        assert!(
            script
                .requests
                .iter()
                .all(|request| RESOURCE_TOOLS.iter().all(|name| !declares(request, name)))
        );
        let messages = script.requests[0]["messages"].to_string();
        assert!(messages.contains("Call the tools of `codemode` servers from codemode scripts."));
        assert!(!messages.contains("Load the tools of `tool_search` servers"));
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn auto_codemode_false_preserves_explicit_selection_and_does_not_block_deferred_search() {
    for (policy, tools, codemode, search) in [
        (json!({"exposure":"codemode"}), vec!["read"], false, false),
        (
            json!({"exposure":"codemode"}),
            vec!["read", "codemode"],
            true,
            false,
        ),
        (json!({"exposure":"deferred"}), vec!["read"], false, true),
    ] {
        let fixture = Fixture::configured(false, policy, Some(false), Some(&tools)).await;
        fixture.assert_active("codemode", codemode);
        fixture.assert_active("tool_search", search);
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn auto_codemode_uses_enabled_configuration_including_unmatched_overrides_and_resources() {
    for (resource_only, policy, expected) in [
        (false, json!({}), true),
        (true, json!({}), true),
        (false, json!({"enabled":false}), false),
        (false, json!({"exposure":"direct"}), false),
        (false, json!({"exposure":"hidden"}), false),
        (false, json!({"exposure":"deferred"}), false),
        (
            false,
            json!({"exposure":"codemode", "enabled":false}),
            false,
        ),
        (
            false,
            json!({"exposure":"direct", "toolExposure":{"future*":"codemode"}}),
            true,
        ),
        (true, json!({"exposure":"codemode"}), true),
    ] {
        let fixture = Fixture::configured(resource_only, policy, Some(true), Some(&["read"])).await;
        fixture.assert_active("codemode", expected);
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn auto_codemode_reload_and_resume_apply_policy_to_restored_selection() {
    let mut fixture = Fixture::configured(
        false,
        json!({"exposure":"codemode"}),
        Some(false),
        Some(&["read"]),
    )
    .await;
    fixture.assert_active("codemode", false);
    fixture.set_auto_enable(json!(true));
    fixture.session.reload().await.unwrap();
    fixture.assert_active("codemode", true);
    assert!(!fixture.session.current().history().path().exists());

    fixture.set_auto_enable(json!(false));
    fixture.session.reload().await.unwrap();
    fixture.assert_active("codemode", true);
    fixture
        .session
        .current()
        .set_active_tools(["read"])
        .unwrap();
    fixture.session.reload().await.unwrap();
    fixture.assert_active("codemode", false);

    fixture
        .session
        .current()
        .prompt("Save this session.")
        .await
        .unwrap();
    let path = fixture.session.current().history().path().to_path_buf();
    fixture
        .pi
        .sessions()
        .close_session(&fixture.session)
        .await
        .unwrap();
    fixture.set_auto_enable(json!(true));
    fixture.session = fixture
        .pi
        .sessions()
        .open_session(&path, Default::default())
        .await
        .unwrap();
    fixture.assert_active("codemode", true);
    let records = fixture
        .session
        .current()
        .history()
        .branch_entries()
        .unwrap();
    assert!(
        records
            .iter()
            .rev()
            .find_map(|record| match &record.entry {
                SessionEntry::ActiveToolsChange(selection) =>
                    Some(selection.active_tool_names.contains(&"codemode".into())),
                _ => None,
            })
            .unwrap()
    );

    fixture
        .session
        .current()
        .set_active_tools(["read"])
        .unwrap();
    fixture
        .session
        .current()
        .prompt("Save a fork point with the reduced selection.")
        .await
        .unwrap();
    let fork_point = fixture
        .session
        .current()
        .history()
        .branch_entries()
        .unwrap()
        .into_iter()
        .rev()
        .find(|record| matches!(record.entry, SessionEntry::Message(_)))
        .unwrap()
        .id;
    fixture
        .session
        .fork_session(fork_point, pi_session::ForkPosition::At)
        .await
        .unwrap();
    fixture.assert_active("codemode", true);

    let previous = fixture.session.current();
    fixture.set_auto_enable(json!("invalid"));
    assert!(
        fixture
            .session
            .reload()
            .await
            .unwrap_err()
            .to_string()
            .contains("autoEnableCodemode")
    );
    assert!(Arc::ptr_eq(&previous, &fixture.session.current()));
    fixture.assert_active("codemode", true);
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_child_discovery_respects_grants_across_reload_and_restore() {
    use pi_core::CustomMessageContent;
    use pi_plugin::{IsolatedSessionOptions, IsolatedSessionRequest};

    for (policy, entry) in [
        (json!({}), "codemode"),
        (json!({"exposure":"deferred"}), "tool_search"),
    ] {
        let fixture = Fixture::configured(false, policy, None, Some(&["read"])).await;
        for tools in [vec![], vec!["read"], vec!["read", entry]] {
            let permitted = tools.contains(&entry);
            let id = fixture
                .session
                .launch_isolated_session(
                    IsolatedSessionRequest::new(CustomMessageContent::Text(
                        "Inspect independently.".into(),
                    ))
                    .options(IsolatedSessionOptions {
                        active_tools: Some(tools.into_iter().map(String::from).collect()),
                        ..Default::default()
                    }),
                )
                .await
                .unwrap();
            let outcome = fixture
                .session
                .wait_for_isolated_session(&id)
                .await
                .unwrap();
            let child = fixture
                .pi
                .sessions()
                .sessions()
                .into_iter()
                .find(|session| session.id() == outcome.session_id)
                .unwrap();
            let assert_entry = || {
                let current = child.current();
                assert_eq!(
                    current
                        .catalog()
                        .tool_specs()
                        .iter()
                        .any(|tool| tool.name == entry),
                    permitted
                );
                assert_eq!(
                    current
                        .snapshot()
                        .agent
                        .active_tools
                        .iter()
                        .any(|name| name == entry),
                    permitted
                );
            };
            assert_entry();
            if permitted {
                child.current().set_active_tools(["read"]).unwrap();
            } else {
                assert!(child.current().set_active_tools([entry]).is_err());
            }
            child.reload().await.unwrap();
            assert_entry();
            if permitted {
                if entry == "codemode" {
                    fixture.set_script([(
                        "codemode",
                        json!({"code":
                            "text(await tools.mcp__docs__search({query: 'child'}));"
                        }),
                    )]);
                } else {
                    fixture.set_script([
                        ("tool_search", json!({"query":"search the docs", "limit":1})),
                        (SEARCH, json!({"query":"child"})),
                    ]);
                }
                child
                    .current()
                    .prompt("Use the permitted discovery entry.")
                    .await
                    .unwrap();
                assert_eq!(fixture.remote_calls.lock().unwrap().len(), 1);
                child.current().set_active_tools(["read"]).unwrap();
            }
            let path = child.path();
            fixture.pi.sessions().close_session(&child).await.unwrap();
            let restored_id = fixture
                .session
                .restore_isolated_session(&path)
                .await
                .unwrap();
            let restored = fixture
                .pi
                .sessions()
                .sessions()
                .into_iter()
                .find(|session| session.path() == path)
                .unwrap();
            assert_eq!(
                restored
                    .current()
                    .snapshot()
                    .agent
                    .active_tools
                    .iter()
                    .any(|name| name == entry),
                permitted
            );
            assert_eq!(
                restored
                    .current()
                    .catalog()
                    .tool_specs()
                    .iter()
                    .any(|tool| tool.name == entry),
                permitted
            );
            assert!(!restored_id.as_str().is_empty());
            fixture
                .pi
                .sessions()
                .close_session(&restored)
                .await
                .unwrap();
        }
        fixture.shutdown().await;
    }
}
