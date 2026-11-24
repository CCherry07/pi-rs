//! Real Coding/model-catalog/provider/session coverage for codemode's model API.
//!
//! Oracles: legacy/pi/packages/coding-agent/src/extensions/codemode/execute.ts,
//! packages/ai/src/api/{typesafe-system-one,openrouter-images}.ts.

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use pi_coding::{Config, Features, Pi};
use pi_core::{ContentBlock, Message, ToolResultMessage};
use pi_session::{PiSession, PromptOutcome, SessionEntry};
use serde_json::{Value, json};

const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

const QUESTIONS: &str = r#"{
    approved: { type: "bool", instructions: "Approval?", criteria: { true: "yes", false: "no" } },
    kind: { type: "choice", instructions: "Choose.", criteria: { good: "Good", bad: "Bad" } },
    urgency: { type: "score", instructions: "Score.", criteria: ["low", "high"] }
}"#;

#[derive(Default)]
struct Script {
    code: Option<String>,
    next_call: usize,
    requests: Vec<Value>,
}

struct ModelRequest {
    path: &'static str,
    body: Value,
    headers: HeaderMap,
}

#[derive(Default)]
struct ServerState {
    script: Mutex<Script>,
    requests: Mutex<Vec<ModelRequest>>,
    held: AtomicBool,
    active: AtomicUsize,
    peak: AtomicUsize,
    changed: tokio::sync::Notify,
}

impl ServerState {
    async fn wait_for_requests(&self, count: usize) {
        loop {
            let changed = self.changed.notified();
            if self.requests.lock().unwrap().len() >= count {
                return;
            }
            changed.await;
        }
    }

    fn release(&self) {
        self.held.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }
}

async fn chat_response(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
    axum::Json(request): axum::Json<Value>,
) -> impl IntoResponse {
    let (delta, reason) = {
        let mut script = state.script.lock().unwrap();
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

async fn classifier_response(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<Value>,
) -> axum::response::Response {
    let text = request["state"]["text"].as_str().unwrap().to_owned();
    state.requests.lock().unwrap().push(ModelRequest {
        path: "classifier",
        body: request,
        headers,
    });
    let active = state.active.fetch_add(1, Ordering::AcqRel) + 1;
    state.peak.fetch_max(active, Ordering::AcqRel);
    state.changed.notify_waiters();
    if text == "hold" {
        loop {
            let changed = state.changed.notified();
            if !state.held.load(Ordering::Acquire) {
                break;
            }
            changed.await;
        }
    } else if text == "wait-for-hold" {
        state.wait_for_requests(2).await;
    }
    state.active.fetch_sub(1, Ordering::AcqRel);
    if text == "explode" {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":{"message":"fixture rejected classification"}})),
        )
            .into_response();
    }
    axum::Json(json!({
        "answers":{
            "approved":{"type":"noul","noul":if text == "bad" {0.1} else {0.9}},
            "kind":{"type":"choice","choice":"good","probabilities":{"good":0.9,"bad":0.1},"confidence":0.9},
            "urgency":{"type":"score","score":1,"confidence":0.8}
        },
        "usage":{"input_tokens":100,"output_tokens":20}
    }))
    .into_response()
}

async fn images_response(
    axum::extract::State(state): axum::extract::State<Arc<ServerState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<Value>,
) -> axum::response::Response {
    let prompt = request["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    state.requests.lock().unwrap().push(ModelRequest {
        path: "image",
        body: request,
        headers,
    });
    state.changed.notify_waiters();
    if prompt == "explode" {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":{"message":"fixture rejected image generation"}})),
        )
            .into_response();
    }
    axum::Json(json!({
        "id":"painted",
        "choices":[{"message":{
            "content":format!("painted {prompt}"),
            "images":[{"image_url":{"url":format!("data:image/png;base64,{TINY_PNG}")}}]
        }}],
        "usage":{"prompt_tokens":200,"completion_tokens":30}
    }))
    .into_response()
}

struct Fixture {
    directory: tempfile::TempDir,
    base: String,
    pi: Pi,
    session: PiSession,
    state: Arc<ServerState>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        Self::configured(|base| model_config(base, "judge"), None).await
    }

    async fn configured(models: impl FnOnce(&str) -> Value, credentials: Option<Value>) -> Self {
        let state = Arc::new(ServerState::default());
        let router = axum::Router::new()
            .route("/v1/chat/completions", axum::routing::post(chat_response))
            .route(
                "/classifier/systemone",
                axum::routing::post(classifier_response),
            )
            .route(
                "/images/chat/completions",
                axum::routing::post(images_response),
            )
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let cwd = directory.path().join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(agent.join("models.json"), models(&base).to_string()).unwrap();
        if let Some(credentials) = credentials {
            fs::write(agent.join("auth.json"), credentials.to_string()).unwrap();
        }
        let mut config = Config::new(cwd.clone(), agent.clone());
        config.features = Features::none();
        config.provider = "fixture".into();
        config.model = Some("test".into());
        config.discover_extensions = false;
        config.trust_override = Some(false);
        let pi = Pi::builder(config).build().unwrap();
        let session = pi
            .sessions()
            .create_session(pi_session::CreateSessionOptions::new(
                &cwd,
                agent.join("session.jsonl"),
            ))
            .await
            .unwrap();
        Self {
            directory,
            base,
            pi,
            session,
            state,
            server,
        }
    }

    async fn run(&self, code: &str) -> ToolResultMessage {
        {
            let mut script = self.state.script.lock().unwrap();
            script.code = Some(code.into());
            script.requests.clear();
        }
        let PromptOutcome::Agent(outcome) = self
            .session
            .current()
            .prompt("Run the model script.")
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

    fn replace_classifier(&self, id: &str) {
        fs::write(
            self.directory.path().join("agent/models.json"),
            model_config(&self.base, id).to_string(),
        )
        .unwrap();
    }

    async fn shutdown(self) {
        self.state.release();
        self.pi.sessions().shutdown().await.unwrap();
        self.server.abort();
    }
}

#[tokio::test]
async fn stock_typed_catalogs_use_stored_credentials_without_replacing_chat_models() {
    let credentials = |typesafe_key: &str| {
        json!({
            "typesafe":{"type":"api_key","key":typesafe_key},
            "openai":{"type":"api_key","key":"stock-openai-fixture"}
        })
    };
    let fixture = Fixture::configured(
        |base| {
            json!({"providers":{
                "fixture":{"baseUrl":format!("{base}/v1"),"api":"openai-completions","apiKey":"fixture",
                    "models":[{"id":"test","contextWindow":100000,"maxTokens":1024}]},
                "typesafe":{"baseUrl":format!("{base}/classifier"),"headers":{"X-Provider-Route":"stock"}}
            }})
        },
        Some(credentials("stock-typesafe-first")),
    )
    .await;
    let code = format!(
        r#"
        const model = await models.getModelOfType("classifier", "typesafe", "jev-latest");
        const openai = await models.getModelOfType("classifier", "openai", "gpt-6-luna");
        const classifiers = await models.getModelsOfType("classifier", "openrouter");
        const images = await models.getModelsOfType("image", "openrouter");
        const chats = await models.getModelsOfType("chat");
        const classify = await models.classify({{...model,
            baseUrl:"http://127.0.0.1:1/ignored", headers:{{Authorization:"injected", "X-Provider-Route":"injected"}}
        }}, {{state:{{text:"good"}}, questions:{QUESTIONS}}});
        return {{
            typesafe: [model.type, model.api, model.id],
            openai: [openai.type, openai.api, openai.id],
            classifiers: classifiers.map(m => [m.id, m.api]),
            images: images.map(m => [m.id, m.api]),
            availableTypesafe: (await models.getAvailableOfType("classifier", "typesafe")).map(m => m.id),
            availableOpenai: (await models.getAvailableOfType("classifier", "openai")).map(m => m.id),
            headers: "headers" in model, apiKey: "apiKey" in model,
            chatOnly: chats.every(m => m.type === "chat" && !["typesafe-system-one", "openai-decisions", "openrouter-images"].includes(m.api)),
            typesafeChats: chats.filter(m => m.provider === "typesafe").length,
            openaiChats: chats.some(m => m.provider === "openai"),
            openrouterChats: chats.some(m => m.provider === "openrouter"),
            stopReason: classify.stopReason, probability: classify.answers.approved.probability
        }};
    "#
    );
    let result = fixture.run(&code).await;
    let value = returned(&result);
    assert_eq!(
        value["typesafe"],
        json!(["classifier", "typesafe-system-one", "jev-latest"])
    );
    assert_eq!(
        value["openai"],
        json!(["classifier", "openai-decisions", "gpt-6-luna"])
    );
    assert_eq!(value["availableTypesafe"], json!(["jev-latest"]));
    assert_eq!(value["availableOpenai"], json!(["gpt-6-luna"]));
    let classifiers = value["classifiers"].as_array().unwrap();
    for id in ["~typesafe/jev-latest", "typesafe/jev-1.14"] {
        assert!(classifiers.contains(&json!([id, "typesafe-system-one"])));
    }
    let images = value["images"].as_array().unwrap();
    for id in ["google/gemini-3.1-flash-image", "openai/gpt-image-2"] {
        assert!(images.contains(&json!([id, "openrouter-images"])));
    }
    assert_eq!(value["headers"], false);
    assert_eq!(value["apiKey"], false);
    assert_eq!(value["chatOnly"], true);
    assert_eq!(value["typesafeChats"], 0);
    assert_eq!(value["openaiChats"], true);
    assert_eq!(value["openrouterChats"], true);
    assert_eq!(value["stopReason"], "stop");
    assert_eq!(value["probability"], 0.9);
    {
        let requests = fixture.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body["model"], "jev-latest");
        assert_eq!(
            requests[0].headers["authorization"],
            "Bearer stock-typesafe-first"
        );
        assert_eq!(requests[0].headers["x-provider-route"], "stock");
    }
    // Authentication is resolved for each request; catalog metadata and the
    // generation do not retain the old API key after auth.json is updated.
    fs::write(
        fixture.directory.path().join("agent/auth.json"),
        credentials("stock-typesafe-second").to_string(),
    )
    .unwrap();
    let second = fixture.run(&code).await;
    assert_eq!(returned(&second)["stopReason"], "stop");
    {
        let requests = fixture.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].headers["authorization"],
            "Bearer stock-typesafe-second"
        );
    }
    assert!(
        fixture
            .state
            .script
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|request| request["model"] == "test")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn availability_rereads_credentials_without_reloading_or_refreshing_oauth() {
    let fixture = Fixture::configured(|base| model_config(base, "judge"), None).await;
    let code = r#"
        return {
            registered: (await models.getModelsOfType("classifier", "unavailable")).map(m => m.id),
            available: (await models.getAvailableOfType("classifier", "unavailable")).map(m => m.id),
            openai: (await models.getAvailableOfType("classifier", "openai")).map(m => m.id)
        };
    "#;
    let first = returned(&fixture.run(code).await);
    assert_eq!(first["registered"], json!(["locked"]));
    assert_eq!(first["available"], json!([]));
    let path = fixture.directory.path().join("agent/auth.json");
    fs::write(
        &path,
        json!({
            "unavailable": {"type":"api_key","key":"test-only-new-key"},
            "openai": {"type":"api_key","key":"test-only-openai"}
        })
        .to_string(),
    )
    .unwrap();
    let second = returned(&fixture.run(code).await);
    assert_eq!(second["available"], json!(["locked"]));
    assert!(
        second["openai"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == "gpt-6-luna")
    );
    // An expired subscription is recognizable without any token refresh. It
    // must not make the API-key-only Decisions models available.
    let oauth = json!({"openai": {
        "type":"oauth","access":"test-only-expired","refresh":"must-not-refresh",
        "expires":0,"clientId":"test-only-client"
    }})
    .to_string();
    fs::write(&path, &oauth).unwrap();
    let third = returned(&fixture.run(code).await);
    assert_eq!(third["registered"], first["registered"]);
    assert_eq!(third["available"], json!([]));
    assert_eq!(third["openai"], json!([]));
    assert_eq!(fs::read_to_string(&path).unwrap(), oauth);
    assert!(!path.with_extension("json.lock").exists());
    assert!(fixture.state.requests.lock().unwrap().is_empty());
    fixture.shutdown().await;
}

#[tokio::test]
async fn copilot_availability_updates_filters_without_removing_registered_models() {
    let credentials = |ids: Value| {
        json!({"github-copilot": {
            "type":"oauth","access":"test-only-copilot","refresh":"must-not-refresh", "expires":0,
            "availableModelIds":ids
        }})
    };
    let fixture = Fixture::configured(
        |base| model_config(base, "judge"),
        Some(credentials(json!(["gemini-3.8-flash"]))),
    )
    .await;
    let code = r#"return {
        registered: (await models.getModelsOfType('chat','github-copilot')).map(m => m.id),
        available: (await models.getAvailableOfType('chat','github-copilot')).map(m => m.id)
    };"#;
    let first = returned(&fixture.run(code).await);
    let registered: std::collections::BTreeSet<_> = first["registered"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    let expected = pi_plugin_copilot::github_copilot_models()
        .into_iter()
        .map(|model| model.id.to_string())
        .collect();
    assert_eq!(registered, expected);
    assert!(registered.contains("gpt-5.6-sol")); // Registered but denied by this account.
    assert_eq!(first["available"], json!(["gemini-3.8-flash"]));
    let path = fixture.directory.path().join("agent/auth.json");
    fs::write(&path, credentials(json!([])).to_string()).unwrap();
    let second = returned(&fixture.run(code).await);
    assert_eq!(second["registered"], first["registered"]);
    assert_eq!(second["available"], json!([]));
    fs::write(&path, credentials(json!(null)).to_string()).unwrap();
    let third = returned(&fixture.run(code).await);
    assert_eq!(third["available"], third["registered"]);
    fs::write(&path, credentials(json!(["gpt-5.6-sol"])).to_string()).unwrap();
    let rotated = returned(&fixture.run(code).await);
    assert_eq!(rotated["registered"], first["registered"]);
    assert_eq!(rotated["available"], json!(["gpt-5.6-sol"]));
    fixture.session.reload().await.unwrap();
    assert_eq!(returned(&fixture.run(code).await), rotated);
    assert!(!path.with_extension("json.lock").exists());
    assert!(fixture.state.requests.lock().unwrap().is_empty());
    fixture.shutdown().await;
}

fn model_config(base: &str, classifier_id: &str) -> Value {
    json!({"providers":{
        "fixture":{"baseUrl":format!("{base}/v1"),"api":"openai-completions","apiKey":"fixture",
            "models":[{"id":"test","input":["text","image"],"contextWindow":100000,"maxTokens":1024}]},
        "scorer":{"baseUrl":format!("{base}/classifier"),"api":"typesafe-system-one","apiKey":"classifier-fixture",
            "headers":{"X-Provider-Route":"registered"},
            "models":[{"type":"classifier","id":classifier_id,"input":["text","image"],"contextWindow":100000,
                "headers":{"X-Model-Route":"registered"},"cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}]},
        "painter":{"baseUrl":format!("{base}/images"),"api":"openrouter-images","apiKey":"image-fixture",
            "models":[{"type":"image","id":"painter","input":["text","image"],"output":["image","text"],
                "cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}]},
        "unavailable":{"baseUrl":format!("{base}/classifier"),"api":"typesafe-system-one",
            "models":[{"type":"classifier","id":"locked","input":["text"],"contextWindow":100000}]}
    }})
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

fn returned(result: &ToolResultMessage) -> Value {
    assert!(!result.is_error, "{}", output(result));
    let returned = result
        .content
        .iter()
        .rev()
        .find_map(|block| match block {
            ContentBlock::Text(text) => Some(&text.text),
            _ => None,
        })
        .expect("the script must return a JSON value");
    serde_json::from_str(returned).unwrap()
}

#[track_caller]
fn assert_cost(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-10,
        "expected cost {expected}, got {actual}"
    );
}

#[tokio::test]
async fn typed_catalog_classification_and_usage_use_registered_routing() {
    let fixture = Fixture::new().await;
    let code = format!(
        r#"
        const [model] = await models.getAvailableOfType("classifier", "scorer");
        const listed = await models.getModelsOfType("classifier");
        const available = await models.getAvailableOfType("classifier");
        const chats = await models.getModelsOfType("chat", "fixture");
        const images = await models.getModelsOfType("image", "painter");
        const results = await Promise.all(["good", "bad"].map(text => models.classify(
            {{...model, baseUrl:"http://127.0.0.1:1/ignored", headers:{{Authorization:"injected"}}}},
            {{state:{{text}}, questions:{QUESTIONS}}}
        )));
        return {{
            same: (await models.getModelOfType("classifier", "scorer", "judge")).id,
            missing: (await models.getModelOfType("classifier", "scorer", "missing")) === undefined,
            headers: "headers" in model, apiKey: "apiKey" in model,
            listed: listed.filter(m => ["scorer", "unavailable"].includes(m.provider)).map(m => m.provider),
            unavailable: available.some(m => m.provider === "unavailable"),
            chats: chats.map(m => [m.type, m.id]), images: images.map(m => [m.type, m.id]),
            scorerChats: (await models.getModelsOfType("chat", "scorer")).length,
            results
        }};
    "#
    );
    let result = fixture.run(&code).await;
    let value = returned(&result);
    assert_eq!(value["same"], "judge");
    assert_eq!(value["missing"], true);
    assert_eq!(value["headers"], false);
    assert_eq!(value["apiKey"], false);
    assert_eq!(value["listed"], json!(["scorer", "unavailable"]));
    assert_eq!(value["unavailable"], false);
    assert_eq!(value["chats"], json!([["chat", "test"]]));
    assert_eq!(value["images"], json!([["image", "painter"]]));
    assert_eq!(value["scorerChats"], 0);
    for (index, probability) in [0.9, 0.1].into_iter().enumerate() {
        let classification = &value["results"][index];
        assert_eq!(classification["stopReason"], "stop");
        assert_eq!(
            classification["answers"]["approved"],
            json!({"type":"bool","probability":probability})
        );
        assert_eq!(classification["answers"]["kind"]["choice"], "good");
        assert_eq!(classification["answers"]["urgency"]["score"], 1.0);
        assert_cost(
            classification["usage"]["cost"]["total"].as_f64().unwrap(),
            0.00014,
        );
    }
    {
        let requests = fixture.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(request.path, "classifier");
            assert_eq!(
                request.headers["authorization"],
                "Bearer classifier-fixture"
            );
            assert_eq!(request.headers["x-provider-route"], "registered");
            assert_eq!(request.headers["x-model-route"], "registered");
            assert_eq!(request.body["model"], "judge");
            assert_eq!(request.body["questions"]["approved"]["type"], "noul");
        }
    }
    let calls = result.details.as_ref().unwrap()["calls"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call["name"], "models.classify");
        assert_eq!(call["args"], "scorer/judge");
        assert_eq!(call["status"], "ok");
        assert_cost(call["cost"].as_f64().unwrap(), 0.00014);
    }
    let usage = result.usage.as_ref().unwrap();
    assert_eq!(usage.input, 200);
    assert_eq!(usage.output, 40);
    assert_cost(usage.cost.total, 0.00028);
    assert_cost(
        fixture.session.current().history().stats().cost_total,
        0.00028,
    );
    let history = fixture.session.current().history().load().unwrap();
    let tool_results = history
        .entries
        .iter()
        .filter_map(|record| match &record.entry {
            SessionEntry::Message(message) => match message.message.as_standard() {
                Some(Message::ToolResult(result)) => Some(result),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].tool_name, "codemode");
    fixture.shutdown().await;
}

#[tokio::test]
async fn model_calls_limit_concurrency_to_four() {
    let fixture = Fixture::new().await;
    fixture.state.held.store(true, Ordering::Release);
    let code = format!(
        r#"
        const model = await models.getModelOfType("classifier", "scorer", "judge");
        const results = await Promise.all(Array.from({{length:6}}, () =>
            models.classify(model, {{state:{{text:"hold"}}, questions:{QUESTIONS}}})));
        return results.map(result => result.stopReason);
    "#
    );
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let run = fixture.run(&code);
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("script completed before held calls were released: {}", output(&result)),
            () = fixture.state.wait_for_requests(4) => {}
        }
        tokio::select! {
            result = &mut run => panic!("script completed before release: {}", output(&result)),
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        assert_eq!(fixture.state.requests.lock().unwrap().len(), 4, "extra calls must wait before reaching the provider");
        fixture.state.release();
        run.await
    }).await.expect("four calls must enter concurrently without a deadlock");
    assert_eq!(
        returned(&result),
        json!(["stop", "stop", "stop", "stop", "stop", "stop"])
    );
    assert_eq!(fixture.state.peak.load(Ordering::Acquire), 4);
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 6);
    assert_cost(result.usage.as_ref().unwrap().cost.total, 0.00084);
    fixture.shutdown().await;
}

#[tokio::test]
async fn image_results_are_displayable_and_provider_failures_are_values() {
    let fixture = Fixture::new().await;
    let code = format!(
        r#"
        const model = await models.getModelOfType("image", "painter", "painter");
        const result = await models.generateImages({{...model, baseUrl:"http://127.0.0.1:1/ignored"}}, {{input:[
            {{type:"text", text:"a fox"}}, {{type:"image", data:"{TINY_PNG}", mimeType:"image/png"}}
        ]}});
        for (const block of result.output) if (block.type === "image") image(block);
        const failed = await models.generateImages(model, {{input:[{{type:"text",text:"explode"}}]}});
        const classifierFailure = await models.classify({{provider:"scorer",id:"judge"}},
            {{state:{{text:"explode"}}, questions:{QUESTIONS}}});
        return {{reason:result.stopReason, text:result.output.find(b => b.type === "text").text,
            failed:[failed.stopReason,failed.errorMessage],
            classifierFailure:[classifierFailure.stopReason,classifierFailure.errorMessage]}};
    "#
    );
    let result = fixture.run(&code).await;
    let value = returned(&result);
    assert_eq!(value["reason"], "stop");
    assert_eq!(value["text"], "painted a fox");
    assert_eq!(value["failed"][0], "error");
    assert!(
        value["failed"][1]
            .as_str()
            .unwrap()
            .contains("fixture rejected image generation")
    );
    assert_eq!(value["classifierFailure"][0], "error");
    assert!(
        value["classifierFailure"][1]
            .as_str()
            .unwrap()
            .contains("fixture rejected classification")
    );
    let images = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image(image) => Some(image),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].data, TINY_PNG);
    assert_eq!(images[0].mime_type, "image/png");
    assert!(!output(&result).contains("did not show"));
    {
        let requests = fixture.state.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].path, "image");
        assert_eq!(requests[0].headers["authorization"], "Bearer image-fixture");
        assert_eq!(requests[0].body["modalities"], json!(["image", "text"]));
        assert_eq!(
            requests[0].body["messages"][0]["content"][1]["image_url"]["url"],
            format!("data:image/png;base64,{TINY_PNG}")
        );
    }
    assert_cost(result.usage.as_ref().unwrap().cost.total, 0.00026);
    fixture.shutdown().await;
}

#[tokio::test]
async fn validation_rejects_before_remote_calls_and_unshown_images_have_a_reminder() {
    let fixture = Fixture::new().await;
    let code = format!(
        r#"
        const classifier = {{provider:"scorer",id:"judge"}};
        const painter = {{provider:"painter",id:"painter"}};
        const attempts = [
            () => models.getModelsOfType("audio"),
            () => models.classify(classifier, []),
            () => models.classify(classifier, {{state:[], questions:{QUESTIONS}}}),
            () => models.classify(classifier, {{state:{{}}, questions:{{}}}}),
            () => models.classify(classifier, {{state:{{}}, questions:{{q:{{type:"bool",instructions:5,criteria:{{true:"yes",false:"no"}}}}}}}}),
            () => models.classify(classifier, {{state:{{}}, questions:{{q:{{type:"bool",instructions:"Approval?",criteria:["yes","no"]}}}}}}),
            () => models.classify(painter, {{state:{{}}, questions:{QUESTIONS}}}),
            () => models.generateImages(painter, []),
            () => models.generateImages(painter, {{input:[]}}),
            () => models.generateImages(painter, {{input:[{{type:"image",data:"bad"}}]}}),
            () => models.generateImages(classifier, {{input:[{{type:"text",text:"fox"}}]}}),
        ];
        return await Promise.all(attempts.map(async attempt => {{
            try {{ await attempt(); return "accepted"; }} catch (error) {{ return typeof error.message === "string"; }}
        }}));
    "#
    );
    let result = fixture.run(&code).await;
    assert_eq!(
        returned(&result),
        json!([
            true, true, true, true, true, true, true, true, true, true, true
        ])
    );
    assert!(fixture.state.requests.lock().unwrap().is_empty());
    assert_eq!(result.details.as_ref().unwrap()["calls"], json!([]));
    let reminder = fixture
        .run(
            r#"
        const result = await models.generateImages({provider:"painter",id:"painter"},
            {input:[{type:"text",text:"a fox"}]});
        return result.stopReason;
    "#,
        )
        .await;
    assert!(!reminder.is_error, "{}", output(&reminder));
    assert!(
        output(&reminder)
            .contains("models.generateImages() returned 1 image that the script did not show")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn abort_preserves_completed_usage_and_cancels_queued_model_calls() {
    let fixture = Fixture::new().await;
    fixture.state.held.store(true, Ordering::Release);
    let code = format!(
        r#"
        const model = await models.getModelOfType("classifier", "scorer", "judge");
        await models.classify(model, {{state:{{text:"good"}}, questions:{QUESTIONS}}});
        await Promise.all(Array.from({{length:6}}, () =>
            models.classify(model, {{state:{{text:"hold"}}, questions:{QUESTIONS}}})));
    "#
    );
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let run = fixture.run(&code);
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("script completed before abort: {}", output(&result)),
            () = fixture.state.wait_for_requests(5) => {}
        }
        fixture.session.current().abort();
        run.await
    })
    .await
    .expect("abort must finish while four provider requests are still held");
    assert!(
        result.is_error,
        "an aborted script must be recorded as a failed tool call"
    );
    let usage = result
        .usage
        .as_ref()
        .expect("completed model usage must survive parent cancellation");
    assert_eq!(usage.input, 100);
    assert_cost(usage.cost.total, 0.00014);
    assert_cost(
        fixture.session.current().history().stats().cost_total,
        0.00014,
    );
    assert_eq!(
        fixture.state.requests.lock().unwrap().len(),
        5,
        "queued model calls must not start after cancellation"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn returning_or_exiting_cancels_started_unawaited_model_calls() {
    for completion in ["return 'done';", "exit();"] {
        let fixture = Fixture::new().await;
        fixture.state.held.store(true, Ordering::Release);
        let code = format!(
            r#"
            const model = await models.getModelOfType("classifier", "scorer", "judge");
            models.classify(model, {{state:{{text:"hold"}}, questions:{QUESTIONS}}});
            await models.classify(model, {{state:{{text:"wait-for-hold"}}, questions:{QUESTIONS}}});
            {completion}
        "#
        );
        let result = tokio::time::timeout(Duration::from_secs(10), fixture.run(&code))
            .await
            .expect("script completion must cancel the started unawaited model call");
        assert!(!result.is_error, "{}", output(&result));
        assert_eq!(fixture.state.requests.lock().unwrap().len(), 2);
        assert_eq!(fixture.state.peak.load(Ordering::Acquire), 2);
        let statuses = result.details.as_ref().unwrap()["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| call["status"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(statuses, ["cancelled", "ok"]);
        assert_cost(result.usage.as_ref().unwrap().cost.total, 0.00014);
        assert_cost(
            fixture.session.current().history().stats().cost_total,
            0.00014,
        );
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn reload_refreshes_typed_models_without_changing_the_chat_selection() {
    let fixture = Fixture::new().await;
    let before = fixture
        .run(r#"return (await models.getAvailableOfType("classifier", "scorer")).map(m => m.id);"#)
        .await;
    assert_eq!(returned(&before), json!(["judge"]));
    fixture.replace_classifier("judge-v2");
    fixture.session.reload().await.unwrap();
    let code = format!(
        r#"
        const model = await models.getModelOfType("classifier", "scorer", "judge-v2");
        const result = await models.classify(model, {{state:{{text:"good"}},questions:{QUESTIONS}}});
        return {{missing:(await models.getModelOfType("classifier","scorer","judge")) === undefined,
            id:model.id, resultModel:result.model}};
    "#
    );
    let after = fixture.run(&code).await;
    assert_eq!(
        returned(&after),
        json!({"missing":true,"id":"judge-v2","resultModel":"judge-v2"})
    );
    assert_eq!(
        fixture.state.requests.lock().unwrap()[0].body["model"],
        "judge-v2"
    );
    assert!(
        fixture
            .state
            .script
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|request| request["model"] == "test")
    );
    fixture.shutdown().await;
}
