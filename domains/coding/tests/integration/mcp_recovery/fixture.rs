use super::*;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pi_coding::{Config, Features, ProductSessionFactory, ProjectTrustService};
use pi_settings::{DefaultProjectTrust, SettingsManager};
use tokio::sync::{Semaphore, watch};

#[derive(Clone, Debug, Default)]
pub(super) struct Trace {
    pub initializes: usize,
    pub lists: Vec<String>,
    pub attempts: Vec<(String, Value)>,
    pub executions: Vec<String>,
    pub requests: Vec<Value>,
}

pub(super) struct Fixture {
    _directory: tempfile::TempDir,
    pub manager: MultiSessionManager,
    pub session: pi_session::PiSession,
    pub generations: Arc<AtomicUsize>,
    pub trace: watch::Sender<Trace>,
    pub expire: Arc<Semaphore>,
    pub finish_turn: Arc<Semaphore>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    pub async fn new(changed: bool) -> Self {
        let trace = watch::channel(Trace::default()).0;
        let expire = Arc::new(Semaphore::new(0));
        let finish_turn = Arc::new(Semaphore::new(0));
        let app = axum::Router::new()
            .route(
                "/mcp",
                axum::routing::post({
                    let trace = trace.clone();
                    let expire = expire.clone();
                    move |headers: HeaderMap, axum::Json(request): axum::Json<Value>| {
                        mcp_response(trace.clone(), expire.clone(), changed, headers, request)
                    }
                })
                .delete(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/v1/chat/completions",
                axum::routing::post({
                    let trace = trace.clone();
                    let finish = finish_turn.clone();
                    move |axum::Json(request): axum::Json<Value>| {
                        provider_response(trace.clone(), finish.clone(), request)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let project = directory.path().join("project");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            agent.join("memory.json"),
            r#"{"version":1,"enabled":false}"#,
        )
        .unwrap();
        std::fs::write(
            agent.join("mcp.json"),
            json!({"mcpServers":{"fixture":{
                "url":format!("{base}/mcp"),"exposure":"direct"
            }}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            agent.join("models.json"),
            json!({"providers":{"fixture":{
                "baseUrl":format!("{base}/v1"),"api":"openai-completions","apiKey":"local-fixture",
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
        config.features = Features::none();
        let (trust, _) =
            ProjectTrustService::new(&agent, Some(false), false, DefaultProjectTrust::Never)
                .unwrap();
        let generations = Arc::new(AtomicUsize::new(0));
        // Use the complete product factory, including its own registered-session
        // binding, context, MCP ownership cache and next-idle admission policy.
        let factory = ProductSessionFactory::new(config, trust, SettingsManager::new(&agent))
            .with_session_binding({
                let generations = generations.clone();
                move |_| {
                    generations.fetch_add(1, Ordering::AcqRel);
                }
            });
        let manager = MultiSessionManager::new(factory);
        let session = manager
            .create_session(CreateSessionOptions::new(
                &project,
                agent.join("recovery.jsonl"),
            ))
            .await
            .unwrap();
        let fixture = Self {
            _directory: directory,
            manager,
            session,
            generations,
            trace,
            expire,
            finish_turn,
            task,
        };
        assert_eq!(fixture.snapshot().initializes, 1);
        assert_eq!(fixture.snapshot().lists, ["connection-1"]);
        assert!(!fixture.session.path().exists(), "startup must remain lazy");
        fixture
    }

    pub fn snapshot(&self) -> Trace {
        self.trace.borrow().clone()
    }

    pub async fn until(&self, predicate: impl Fn(&Trace) -> bool) {
        let mut receiver = self.trace.subscribe();
        tokio::time::timeout(TIMEOUT, receiver.wait_for(predicate))
            .await
            .unwrap_or_else(|_| panic!("missing fixture event: {:?}", self.snapshot()))
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) fn descriptor(changed: bool) -> Value {
    json!({"name":"echo","description":if changed {"Changed callable descriptor"} else {"Original callable descriptor"},
        "inputSchema":{"type":"object","properties":{"value":{"type":if changed {"number"} else {"string"}}},"required":["value"]},
        "outputSchema":{"type":"object","properties":{"value":{"type":if changed {"boolean"} else {"string"}}},"required":["value"]},
        "annotations":{"readOnlyHint":false,"destructiveHint":true},"_meta":{"revision":if changed {2} else {1}}
    })
}

async fn mcp_response(
    trace: watch::Sender<Trace>,
    expire: Arc<Semaphore>,
    changed: bool,
    headers: HeaderMap,
    request: Value,
) -> Response {
    let id = &request["id"];
    if id.is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let connection = headers
        .get("mcp-session-id")
        .map(|id| id.to_str().unwrap().to_owned());
    let mut new_session = None;
    let result = match request["method"].as_str().unwrap() {
        // Normal SDK negotiation: the server genuinely only supports initialize.
        // No client-side forced legacy mode or test-only transport replacement.
        "server/discover" => {
            return axum::Json(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Use initialize"}}),
            )
            .into_response();
        }
        "initialize" => {
            let mut number = 0;
            trace.send_modify(|trace| {
                trace.initializes += 1;
                number = trace.initializes;
            });
            assert!(number <= 2, "recovery may reopen only once");
            new_session = Some(format!("connection-{number}"));
            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"recovery-fixture","version":"1"},
                "instructions":if number == 1 {OLD_GUIDANCE} else {NEW_GUIDANCE}})
        }
        "tools/list" => {
            let connection = connection.expect("stateful list request");
            let replacement = connection == "connection-2";
            trace.send_modify(|trace| trace.lists.push(connection));
            json!({"tools":[descriptor(changed && replacement)]})
        }
        "tools/call" => {
            let connection = connection.expect("stateful tool request");
            assert_eq!(request["params"]["name"], "echo");
            trace.send_modify(|trace| {
                trace
                    .attempts
                    .push((connection.clone(), request["params"]["arguments"].clone()))
            });
            if connection == "connection-1" {
                expire.acquire().await.unwrap().forget();
                // Definitive session-attached404, before any remote execution.
                return (StatusCode::NOT_FOUND, "MCP session expired").into_response();
            }
            assert_eq!(connection, "connection-2");
            // Do not reject old arguments here: recording any dispatch exposes an
            // unsafe replay when the replacement descriptor no longer matches.
            trace.send_modify(|trace| trace.executions.push(connection));
            json!({"content":[{"type":"text","text":"executed once on replacement"}],"structuredContent":{"value":"executed"}})
        }
        method => panic!("unexpected MCP method {method}"),
    };
    let mut response = axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response();
    if let Some(connection) = new_session {
        response
            .headers_mut()
            .insert("mcp-session-id", connection.parse().unwrap());
    }
    response
}

// Deterministic scripted responses over the product's real configured OpenAI
// adapter. ProductSessionFactory has no direct ScriptedProvider injection seam.
async fn provider_response(
    trace: watch::Sender<Trace>,
    finish: Arc<Semaphore>,
    request: Value,
) -> impl IntoResponse {
    let mut number = 0;
    trace.send_modify(|trace| {
        trace.requests.push(request);
        number = trace.requests.len();
    });
    assert!(
        number <= 4,
        "no model retry or extra logical tool execution"
    );
    if number == 2 {
        finish.acquire().await.unwrap().forget();
    }
    let (delta, reason) = if number == 1 {
        (
            json!({"role":"assistant","tool_calls":[{"index":0,"id":CALL_ID,"type":"function","function":{"name":TOOL,"arguments":r#"{"value":"original"}"#}}]}),
            "tool_calls",
        )
    } else {
        (json!({"role":"assistant","content":"done"}), "stop")
    };
    let chunk = json!({"id":"recovery","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
    let end = json!({"id":"recovery","object":"chat.completion.chunk","model":"test","choices":[{"index":0,"delta":{},"finish_reason":reason}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
    )
}
