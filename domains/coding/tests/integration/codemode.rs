//! Codemode crosses the real Coding/MCP/session seams without a live model.
//!
//! Oracle: legacy/pi/packages/coding-agent/src/extensions/codemode/{tool,execute}.ts.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::response::IntoResponse;
use pi_coding::{Config, Features, Pi};
use pi_core::{
    ContentBlock, Message, PluginId, TextContent, ToolCallId, ToolExecutionMode, ToolResult,
    ToolResultMessage, ToolSpec,
};
use pi_plugin::{
    AgentPluginContext, Plugin, PluginError, RegisterContext, Tool, ToolCallBlock, ToolCallEvent,
    ToolCallPatch, ToolContext, ToolError, ToolResultEvent, ToolResultPatch, ToolUpdateSink,
};
use pi_session::{PiSession, PromptOutcome, SessionEntry, SessionGenerationOverlay};
use serde_json::{Value, json};

#[derive(Default)]
struct ProviderScript {
    code: Option<String>,
    requests: Vec<Value>,
    next_call: usize,
}

async fn provider_response(
    axum::extract::State(script): axum::extract::State<Arc<Mutex<ProviderScript>>>,
    axum::Json(request): axum::Json<Value>,
) -> impl IntoResponse {
    let (delta, reason) = {
        let mut script = script.lock().unwrap();
        script.requests.push(request);
        if let Some(code) = script.code.take() {
            script.next_call += 1;
            (
                json!({"role":"assistant","tool_calls":[{
                    "index":0,"id":format!("script-{}",script.next_call),"type":"function",
                    "function":{"name":"codemode","arguments":json!({"code":code}).to_string()}
                }]}),
                "tool_calls",
            )
        } else {
            (json!({"role":"assistant","content":"Done."}), "stop")
        }
    };
    let chunk = json!({"id":"script","object":"chat.completion.chunk","model":"test",
        "choices":[{"index":0,"delta":delta,"finish_reason":null}]});
    let end = json!({"id":"script","object":"chat.completion.chunk","model":"test",
        "choices":[{"index":0,"delta":{},"finish_reason":reason}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
    )
}

struct ObservedCall {
    name: String,
    id: String,
    parent: Option<String>,
}

#[derive(Clone, Default)]
struct HookProbe(Arc<Mutex<Vec<ObservedCall>>>, Arc<CleanupProbe>);

#[derive(Default)]
struct CleanupProbe {
    started: AtomicBool,
    dropped: AtomicBool,
    started_changed: tokio::sync::Notify,
    result_interruptions: Mutex<Vec<bool>>,
}

struct PendingDrop(Arc<CleanupProbe>);

impl Drop for PendingDrop {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::Release);
    }
}

struct CleanupTool {
    probe: Arc<CleanupProbe>,
    pending: bool,
}

#[async_trait]
impl Tool for CleanupTool {
    fn spec(&self) -> ToolSpec {
        let name = if self.pending {
            "codemode_pending"
        } else {
            "codemode_wait_started"
        };
        ToolSpec {
            name: name.into(),
            label: name.into(),
            description: "Deterministic codemode cleanup fixture.".into(),
            parameters: json!({"type":"object"}),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        _id: ToolCallId,
        _input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        if self.pending {
            let _drop = PendingDrop(Arc::clone(&self.probe));
            self.probe.started.store(true, Ordering::Release);
            self.probe.started_changed.notify_one();
            // The scheduler must cancel this execution and dispatch its result
            // hook. The tool deliberately has no independent abort handler.
            return std::future::pending().await;
        }
        loop {
            let changed = self.probe.started_changed.notified();
            if self.probe.started.load(Ordering::Acquire) {
                return Ok(ToolResult::text("child started"));
            }
            changed.await;
        }
    }
}

#[pi_plugin::plugin]
impl Plugin for HookProbe {
    fn id(&self) -> PluginId {
        PluginId::new("codemode-hook-probe")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        for pending in [true, false] {
            context.register_tool(Arc::new(CleanupTool {
                probe: Arc::clone(&self.1),
                pending,
            }))?;
        }
        Ok(())
    }

    async fn tool_call(
        &self,
        _context: AgentPluginContext,
        event: ToolCallEvent,
    ) -> Result<ToolCallPatch, PluginError> {
        self.0.lock().unwrap().push(ObservedCall {
            name: event.tool_call.name.clone(),
            id: event.tool_call.id.as_str().to_owned(),
            parent: event.parent_tool_call_id.map(|id| id.as_str().to_owned()),
        });
        if event.tool_call.name == "read" && event.validated_args["path"] == "redirect.txt" {
            return Ok(ToolCallPatch {
                arguments: Some(json!({"path":"document.txt"})),
                ..Default::default()
            });
        }
        if event.tool_call.name == "grep" && event.validated_args["pattern"] == "BLOCKED" {
            return Ok(ToolCallPatch {
                block: Some(ToolCallBlock {
                    reason: "blocked by fixture policy".into(),
                    terminate: false,
                }),
                ..Default::default()
            });
        }
        Ok(ToolCallPatch::default())
    }

    async fn tool_result(
        &self,
        _context: AgentPluginContext,
        event: ToolResultEvent,
    ) -> Result<ToolResultPatch, PluginError> {
        if event.tool_call.name == "codemode_pending" {
            assert!(event.parent_tool_call_id.is_some());
            self.1
                .result_interruptions
                .lock()
                .unwrap()
                .push(event.interrupted);
        }
        if event.tool_call.name == "read" && event.validated_args["path"] == "document.txt" {
            assert!(
                !event.result.is_error,
                "patched path must reach the read tool"
            );
            return Ok(ToolResultPatch {
                content: Some(vec![ContentBlock::Text(TextContent::new(
                    "result patched by hook",
                ))]),
                ..Default::default()
            });
        }
        Ok(ToolResultPatch::default())
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    pi: Pi,
    session: PiSession,
    script: Arc<Mutex<ProviderScript>>,
    remote_calls: Arc<Mutex<Vec<Value>>>,
    hooks: HookProbe,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let script = Arc::new(Mutex::new(ProviderScript::default()));
        let remote_calls = Arc::new(Mutex::new(Vec::new()));
        let router = axum::Router::new()
            .route("/v1/chat/completions", axum::routing::post(provider_response))
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
                                    return axum::Json(json!({"jsonrpc":"2.0","id":id,
                                        "error":{"code":-32601,"message":"Use initialize"}}))
                                    .into_response();
                                }
                                "initialize" => json!({"protocolVersion":"2025-03-26",
                                    "capabilities":{"tools":{}},
                                    "serverInfo":{"name":"codemode-fixture","version":"1"}}),
                                "tools/list" => json!({"tools":[
                                    {"name":"lookup","description":"Look up a document.",
                                        "inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]},
                                        "annotations":{"readOnlyHint":true}},
                                    {"name":"hidden","description":"Hidden document.","inputSchema":{"type":"object"}}
                                ]}),
                                "tools/call" => {
                                    remote_calls.lock().unwrap().push(request.clone());
                                    json!({"content":[{"type":"text","text":"large intermediate data ".repeat(1024)}],
                                        "structuredContent":{"answer":42},"_meta":{"transportOnly":true}})
                                }
                                method => panic!("unexpected MCP request {method}"),
                            };
                            axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result}))
                                .into_response()
                        }
                    }
                }),
            )
            .with_state(Arc::clone(&script));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let cwd = directory.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            agent.join("models.json"),
            json!({"providers":{"fixture":{"baseUrl":format!("{base}/v1"),
                "api":"openai-completions","apiKey":"fixture",
                "models":[{"id":"test","contextWindow":100000,"maxTokens":1024}]}}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            agent.join("mcp.json"),
            json!({"mcpServers":{"docs":{"url":format!("{base}/mcp"),
                "exposure":"codemode","toolExposure":{"hidden":"hidden"}}}})
            .to_string(),
        )
        .unwrap();
        let mut config = Config::new(cwd.clone(), agent.clone());
        config.features = Features::none();
        config.provider = "fixture".into();
        config.model = Some("test".into());
        config.discover_extensions = false;
        config.trust_override = Some(false);
        let pi = Pi::builder(config).build().unwrap();
        let hooks = HookProbe::default();
        let overlay = SessionGenerationOverlay::new().with_plugin({
            let hooks = hooks.clone();
            move || Arc::new(hooks.clone())
        });
        let session = pi
            .sessions()
            .create_session(pi_session::CreateSessionOptions {
                generation_overlay: overlay,
                ..pi_session::CreateSessionOptions::new(&cwd, agent.join("session.jsonl"))
            })
            .await
            .unwrap();
        Self {
            directory,
            pi,
            session,
            script,
            remote_calls,
            hooks,
            server,
        }
    }

    async fn run(&self, code: &str) -> ToolResultMessage {
        {
            let mut script = self.script.lock().unwrap();
            script.code = Some(code.into());
            script.requests.clear();
        }
        let PromptOutcome::Agent(outcome) = self
            .session
            .current()
            .prompt("Run the script.")
            .await
            .unwrap()
        else {
            panic!("expected agent run");
        };
        let results: Vec<_> = outcome
            .new_messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(result) => Some(result.as_ref().clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            results.len(),
            1,
            "only the outer call enters the model transcript"
        );
        assert_eq!(results[0].tool_name, "codemode");
        results.into_iter().next().unwrap()
    }

    fn write(&self, path: &str, contents: &str) {
        fs::write(self.directory.path().join("project").join(path), contents).unwrap();
    }

    fn store_entries(&self) -> usize {
        self.session.current().history().branch_entries().unwrap().iter().filter(|entry|
            matches!(&entry.entry, SessionEntry::Custom(custom) if custom.custom_type == "codemode-store")
        ).count()
    }

    async fn shutdown(self) {
        self.pi.sessions().shutdown().await.unwrap();
        self.server.abort();
    }
}

fn output(result: &ToolResultMessage) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_pending_child_finalized(fixture: &Fixture) {
    assert!(fixture.hooks.1.started.load(Ordering::Acquire));
    assert!(fixture.hooks.1.dropped.load(Ordering::Acquire));
    assert_eq!(
        *fixture.hooks.1.result_interruptions.lock().unwrap(),
        [true],
        "a started child must receive exactly one interrupted result hook before the outer result"
    );
    let history = fixture.session.current().history().load().unwrap();
    let results: Vec<_> = history
        .entries
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::Message(message) => match message.message.as_standard() {
                Some(Message::ToolResult(result)) => Some(result),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        results.len(),
        1,
        "child cleanup must not append an orphan result"
    );
    assert_eq!(results[0].tool_name, "codemode");
}

#[tokio::test]
async fn returning_or_exiting_a_script_finalizes_started_unawaited_children() {
    for finish in ["return 'finished';", "exit();"] {
        let fixture = Fixture::new().await;
        let code = format!(
            "tools.codemode_pending({{}}); await tools.codemode_wait_started({{}}); {finish}"
        );
        let result = tokio::time::timeout(Duration::from_secs(10), fixture.run(&code))
            .await
            .expect("script cleanup must finish");
        assert!(!result.is_error, "{}", output(&result));
        assert_pending_child_finalized(&fixture);
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn script_timeout_finalizes_started_unawaited_children() {
    let fixture = Fixture::new().await;
    let code = concat!(
        "// @options: {\"timeout_ms\":1000}\n",
        "tools.codemode_pending({}); await tools.codemode_wait_started({}); ",
        "text('child started'); await new Promise(() => {});"
    );
    let result = tokio::time::timeout(Duration::from_secs(10), fixture.run(code))
        .await
        .expect("script timeout and cleanup must finish");
    assert!(result.is_error);
    assert!(output(&result).contains("child started"));
    assert_pending_child_finalized(&fixture);
    fixture.shutdown().await;
}

#[tokio::test]
async fn read_and_grep_demo_combines_real_tools_without_nested_transcript_messages() {
    let fixture = Fixture::new().await;
    fixture.write("policy.rs", "// policy preview\nMcpExposure::Deferred\n");
    let result = fixture
        .run(
            r#"
        const [policy, usages] = await Promise.all([
            tools.read({path: "policy.rs"}),
            tools.grep({path: "policy.rs", pattern: "McpExposure::Deferred", limit: 5}),
        ]);
        text({policyPreview: policy.split("\n")[0], deferredUsages: usages});
    "#,
        )
        .await;
    assert!(!result.is_error, "{}", output(&result));
    assert!(output(&result).contains("policy preview"));
    assert!(output(&result).contains("McpExposure::Deferred"));
    {
        let script = fixture.script.lock().unwrap();
        assert_eq!(script.requests.len(), 2);
        let messages = script.requests[1]["messages"].as_array().unwrap();
        let calls: Vec<_> = messages
            .iter()
            .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "codemode");
        let results: Vec<_> = messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["tool_call_id"], calls[0]["id"]);
    }
    let history = fixture.session.current().history().load().unwrap();
    let saved_results: Vec<_> = history
        .entries
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::Message(message) => match message.message.as_standard() {
                Some(Message::ToolResult(result)) => Some(result),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(saved_results.len(), 1);
    assert_eq!(saved_results[0].tool_name, "codemode");
    fixture.shutdown().await;
}

#[tokio::test]
async fn nested_calls_obey_argument_result_and_permission_hooks() {
    let fixture = Fixture::new().await;
    fixture.write("document.txt", "original tool content");
    let result = fixture
        .run(
            r#"
        text(await tools.read({path: "redirect.txt"}));
        const [blocked] = await Promise.allSettled([
            tools.grep({path: ".", pattern: "BLOCKED"}),
        ]);
        text({status: blocked.status, reason: String(blocked.reason)});
    "#,
        )
        .await;
    assert!(!result.is_error, "{}", output(&result));
    assert!(output(&result).contains("result patched by hook"));
    assert!(output(&result).contains("rejected"));
    assert!(output(&result).contains("blocked by fixture policy"));
    {
        let calls = fixture.hooks.0.lock().unwrap();
        let nested: Vec<_> = calls
            .iter()
            .filter(|call| call.name != "codemode")
            .collect();
        assert_eq!(nested.len(), 2);
        assert!(nested.iter().all(|call| call.id.starts_with("script-1/")));
        assert!(
            nested
                .iter()
                .all(|call| call.parent.as_deref() == Some("script-1"))
        );
        assert_ne!(nested[0].id, nested[1].id);
        assert!(
            calls
                .iter()
                .filter(|call| call.name == "codemode")
                .all(|call| call.parent.is_none())
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_objects_remain_inside_script_and_hidden_tools_remain_unavailable() {
    let fixture = Fixture::new().await;
    let result = fixture
        .run(
            r#"
        const reply = await tools.mcp__docs__lookup({query: "guide"});
        if (reply.structuredContent.answer !== 42 || "_meta" in reply) {
            throw new Error("MCP result projection changed");
        }
        for (const name of ["mcp__docs__hidden", "codemode", "tool_search"]) {
            if (typeof tools[name] !== "undefined") throw new Error("Unexpected nested tool " + name);
        }
        text({answer: reply.structuredContent.answer,
            intermediateBytes: reply.content[0].text.length,
            hasTransportMeta: "_meta" in reply,
            hidden: typeof tools.mcp__docs__hidden,
            recursive: typeof tools.codemode,
            modelOnly: typeof tools.tool_search});
    "#,
        )
        .await;
    let text = output(&result);
    assert!(!result.is_error, "{text}");
    assert!(text.contains("42"));
    assert!(text.contains("undefined"));
    assert!(!text.contains("large intermediate data"));
    assert!(text.contains("false"));
    assert_eq!(fixture.remote_calls.lock().unwrap().len(), 1);
    {
        let script = fixture.script.lock().unwrap();
        let declarations = script.requests[0]["tools"].as_array().unwrap();
        assert!(
            declarations
                .iter()
                .any(|tool| tool["function"]["name"] == "codemode")
        );
        assert!(
            !declarations
                .iter()
                .any(|tool| tool["function"]["name"] == "mcp__docs__lookup")
        );
        assert!(
            !script.requests[1]
                .to_string()
                .contains("large intermediate data")
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn only_successful_store_writes_survive_branch_navigation_reload_and_resume() {
    let mut fixture = Fixture::new().await;
    let result = fixture
        .run("store('value', 'base'); text(load('value')); ")
        .await;
    assert!(!result.is_error, "{}", output(&result));
    let base_leaf = fixture.session.current().history().leaf_id().unwrap();
    fixture
        .run("store('value', 'side'); text(load('value')); ")
        .await;
    let side_leaf = fixture.session.current().history().leaf_id().unwrap();
    fixture
        .session
        .current()
        .checkout(Some(&base_leaf))
        .await
        .unwrap();
    assert!(output(&fixture.run("text(load('value')); ").await).contains("base"));
    fixture
        .session
        .current()
        .checkout(Some(&side_leaf))
        .await
        .unwrap();
    let entries_before_failure = fixture.store_entries();
    let failed = fixture
        .run("store('value', 'discard'); text('partial'); throw new Error('fixture failure');")
        .await;
    assert!(failed.is_error);
    assert!(output(&failed).contains("partial"));
    assert_eq!(fixture.store_entries(), entries_before_failure);
    fixture.session.reload().await.unwrap();
    assert!(output(&fixture.run("text(load('value')); ").await).contains("side"));
    let path = fixture.session.current().history().path().to_path_buf();
    fixture
        .pi
        .sessions()
        .close_session(&fixture.session)
        .await
        .unwrap();
    fixture.session = fixture
        .pi
        .sessions()
        .open_session(&path, Default::default())
        .await
        .unwrap();
    assert!(output(&fixture.run("text(load('value')); ").await).contains("side"));
    let deleted = fixture
        .run("store('value', undefined); text(typeof load('value')); ")
        .await;
    assert!(!deleted.is_error, "{}", output(&deleted));
    fixture.session.reload().await.unwrap();
    assert!(output(&fixture.run("text(typeof load('value')); ").await).contains("undefined"));
    fixture.shutdown().await;
}
