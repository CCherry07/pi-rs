use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::routing::post;
use axum::{Json, Router};
use pi_coding::Config;
use pi_coding_eval::{
    CodingEvalCase, CodingEvalHarness, CodingEvalSystemPrompt, CodingEvalVariant,
};
use pi_eval::{ArtifactStore, EvalCase, EvalFixture, EvalStep, ExactOutputGrader};

#[derive(Default)]
struct RequestCapture {
    requests: Vec<serde_json::Value>,
    bindings: BTreeMap<String, String>,
    bootstrap: BTreeMap<String, serde_json::Value>,
    copied_workspace_entries: Vec<String>,
}

async fn completion(
    State(capture): State<Arc<Mutex<RequestCapture>>>,
    Json(request): Json<serde_json::Value>,
) -> ([(&'static str, &'static str); 1], &'static str) {
    let mut capture = capture.lock().unwrap();
    let prompt = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .unwrap();
    let bindings = prompt
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    if let Some(agent_dir) = bindings.get("agent_dir") {
        for name in ["auth.json", "models.json", "settings.json", "memory.json"] {
            let content = std::fs::read_to_string(PathBuf::from(agent_dir).join(name)).unwrap();
            capture
                .bootstrap
                .insert(name.to_string(), serde_json::from_str(&content).unwrap());
        }
    }
    if let Some(workspace) = bindings.get("workspace") {
        capture.copied_workspace_entries = std::fs::read_dir(workspace)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
    }
    capture.bindings = bindings;
    capture.requests.push(request);
    (
        [(CONTENT_TYPE.as_str(), "text/event-stream")],
        concat!(
            "data: {\"id\":\"chatcmpl-eval\",\"object\":\"chat.completion.chunk\",",
            "\"created\":0,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"role\":\"assistant\",\"content\":\"Paris\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-eval\",\"object\":\"chat.completion.chunk\",",
            "\"created\":0,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,",
            "\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,",
            "\"completion_tokens\":1,\"total_tokens\":9}}\n\n",
            "data: [DONE]\n\n"
        ),
    )
}

struct ProductHarness {
    root: tempfile::TempDir,
    harness: CodingEvalHarness,
    config: Config,
    capture: Arc<Mutex<RequestCapture>>,
    server: tokio::task::JoinHandle<()>,
}

impl ProductHarness {
    async fn new() -> Self {
        let capture = Arc::new(Mutex::new(RequestCapture::default()));
        let router = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(Arc::clone(&capture));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let source_agent = root.path().join("source-agent");
        std::fs::create_dir_all(&source_agent).unwrap();
        let harness =
            CodingEvalHarness::new(ArtifactStore::new(root.path().join("artifacts")).unwrap());
        let mut config = Config::new(root.path().to_path_buf(), source_agent);
        config.provider = "openai-compatible".to_string();
        config.requested_provider = Some(config.provider.clone());
        config.model = Some("gpt-4o-mini".to_string());
        config.base_url = format!("http://{address}/v1");
        config.api_key = Some("test-key".to_string());
        Self {
            root,
            harness,
            config,
            capture,
            server,
        }
    }
}

impl Drop for ProductHarness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn product_run_uses_isolated_settings_and_persists_native_artifacts() {
    let product = ProductHarness::new().await;
    let source_agent = &product.config.agent_dir;
    let auth = serde_json::json!({"fixture": {"type": "api_key", "key": "fixture-key"}});
    let models = serde_json::json!({"providers": {}});
    std::fs::write(source_agent.join("auth.json"), auth.to_string()).unwrap();
    std::fs::write(source_agent.join("models.json"), models.to_string()).unwrap();
    std::fs::write(
        source_agent.join("settings.json"),
        "{\"defaultTools\":[\"bash\"],\"shellCommandPrefix\":\"source-prefix\"}",
    )
    .unwrap();
    std::fs::write(source_agent.join("memory.json"), "{\"enabled\":true}").unwrap();
    let fixture = product.root.path().join("fixture");
    for generated in [".git", "node_modules", "target"] {
        std::fs::create_dir_all(fixture.join(generated)).unwrap();
        std::fs::write(fixture.join(generated).join("generated"), "generated").unwrap();
    }
    std::fs::write(fixture.join("source.txt"), "fixture source").unwrap();
    let case = CodingEvalCase::new(
        EvalCase::new("smoke/basic-answer", "product harness smoke")
            .fixture(EvalFixture::Directory(fixture))
            .step(EvalStep::PromptTemplate(
                "workspace={{workspace}}\nagent_dir={{agent_dir}}\nhome={{home}}".to_string(),
            ))
            .grader(ExactOutputGrader::new("Paris"))
            .active_tools(Vec::<String>::new()),
    );
    let run = product
        .harness
        .run(&case, "candidate", 1, product.config.clone())
        .await
        .unwrap();

    assert!(run.passed, "{run:#?}");
    assert_eq!(run.schema_version, 1);
    assert_eq!(run.provider, "openai-compatible");
    assert_eq!(run.model, "gpt-4o-mini");
    assert_eq!(run.observation.final_response, "Paris");
    assert_eq!(run.observation.usage.total_tokens, 9);
    assert!(
        run.observation
            .system_prompt
            .unwrap()
            .contains("Available tools:\n(none)")
    );
    assert_eq!(
        run.artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect::<Vec<_>>(),
        [
            "observation.json",
            "workspace-changes.json",
            "session.jsonl",
            "run.json"
        ]
    );
    assert!(
        product
            .harness
            .artifacts()
            .root()
            .join("runs.jsonl")
            .exists()
    );

    let capture = product.capture.lock().unwrap();
    assert_eq!(capture.requests.len(), 1);
    assert_eq!(capture.bootstrap["auth.json"], auth);
    assert_eq!(capture.bootstrap["models.json"], models);
    assert_eq!(capture.bootstrap["memory.json"]["enabled"], false);
    let settings = &capture.bootstrap["settings.json"];
    assert_eq!(settings["defaultTools"], serde_json::json!([]));
    let prefix = settings["shellCommandPrefix"].as_str().unwrap();
    assert!(prefix.contains(&capture.bindings["home"]));
    assert!(!prefix.contains("source-prefix"));
    assert_eq!(capture.copied_workspace_entries, ["source.txt"]);
    let home = PathBuf::from(&capture.bindings["home"]);
    assert_eq!(
        PathBuf::from(&capture.bindings["agent_dir"]),
        home.join(".pi/agent")
    );
    assert!(
        !home.exists(),
        "run directories should be removed after shutdown"
    );
    assert_eq!(
        std::fs::read_to_string(source_agent.join("memory.json")).unwrap(),
        "{\"enabled\":true}"
    );
}

#[tokio::test]
async fn documentation_treatment_preserves_append_and_context_across_reload() {
    let product = ProductHarness::new().await;
    let fixture = product.root.path().join("fixture");
    std::fs::create_dir_all(fixture.join(".pi")).unwrap();
    std::fs::write(
        fixture.join(".pi/APPEND_SYSTEM.md"),
        "Keep appended instructions.",
    )
    .unwrap();
    std::fs::write(fixture.join("AGENTS.md"), "Keep project context.").unwrap();
    let case = CodingEvalCase::new(
        EvalCase::new("prompt/treatment", "prompt treatment survives reload")
            .fixture(EvalFixture::Directory(fixture))
            .step(EvalStep::Prompt("Capital of France?".to_string()))
            .step(EvalStep::Reload)
            .step(EvalStep::Prompt("Answer once more.".to_string()))
            .grader(ExactOutputGrader::new("Paris"))
            .active_tools(Vec::<String>::new()),
    );
    let run = product
        .harness
        .run(
            &case,
            CodingEvalVariant::new("without-docs")
                .system_prompt(CodingEvalSystemPrompt::WithoutPiDocumentation),
            1,
            product.config.clone(),
        )
        .await
        .unwrap();
    assert!(run.passed, "{run:#?}");
    let capture = product.capture.lock().unwrap();
    assert_eq!(capture.requests.len(), 2);
    for request in &capture.requests {
        let prompt = request["messages"][0]["content"].as_str().unwrap();
        assert!(!prompt.contains("Pi documentation (read only"));
        assert!(prompt.contains("Keep appended instructions."));
        assert!(prompt.contains("Keep project context."));
        assert!(prompt.contains("Current working directory:"));
    }
    let observed = run.observation.system_prompt.unwrap();
    assert!(observed.contains("Keep appended instructions."));
    assert!(observed.contains("Keep project context."));
}

#[tokio::test]
async fn explicit_native_plugin_paths_reach_the_product_loader_from_original_cwd() {
    let root = tempfile::tempdir().unwrap();
    let source_agent = root.path().join("source-agent");
    std::fs::create_dir_all(&source_agent).unwrap();
    let harness =
        CodingEvalHarness::new(ArtifactStore::new(root.path().join("artifacts")).unwrap());
    let case = CodingEvalCase::new(
        EvalCase::new("native/explicit", "native path")
            .step(EvalStep::Prompt("unused".to_string())),
    );
    let error = harness
        .run(
            &case,
            CodingEvalVariant::new("candidate").native_plugin("native/missing-plugin"),
            1,
            Config::new(root.path().to_path_buf(), source_agent),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains(
            &root
                .path()
                .join("native/missing-plugin")
                .display()
                .to_string()
        )
    );
}

#[tokio::test]
async fn javascript_cases_and_explicit_sources_require_the_node_host() {
    let root = tempfile::tempdir().unwrap();
    let harness =
        CodingEvalHarness::new(ArtifactStore::new(root.path().join("artifacts")).unwrap());
    let case = EvalCase::new("javascript/required", "requires JavaScript")
        .step(EvalStep::Prompt("unused".to_string()));
    for (case, variant) in [
        (
            CodingEvalCase::new(case.clone()).requires_js_host(true),
            CodingEvalVariant::new("candidate"),
        ),
        (
            CodingEvalCase::new(case),
            CodingEvalVariant::new("candidate").extension("extension.ts"),
        ),
    ] {
        let error = harness
            .run(
                &case,
                variant,
                1,
                Config::new(root.path().to_path_buf(), root.path().join("source-agent")),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Node pi-eval launcher"));
    }
}
