//! Actual provider stream -> AgentSession -> JSONL -> resumed provider request.
use std::sync::{Arc, Mutex};

use axum::{Json, Router, extract::State, routing::post};
use pi_agent::AgentOptions;
use pi_core::{Message, ModelSpec, SystemMessage, ThinkingLevel, UserMessage};
use pi_plugin::{PluginId, ProviderPlugin, ProviderRegisterContext};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiResponsesCompatibleProvider};
use pi_runtime::PiRuntime;
use pi_session::{AgentSession, AgentSessionOptions};
use serde_json::{Value, json};

struct FixtureProvider(String);
#[pi_plugin::provider_plugin]
impl ProviderPlugin for FixtureProvider {
    fn id(&self) -> PluginId {
        "history-fixture".into()
    }
    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_provider(Arc::new(
            OpenAiResponsesCompatibleProvider::new(
                OpenAiCompatibleConfig::new(&self.0, "sk-fixture").provider_id("history-fixture"),
            )
            .unwrap(),
        ))?;
        for id in ["model", "other"] {
            let mut model = ModelSpec::new("history-fixture", id, id, "openai-responses");
            model.reasoning = true;
            model.base_url = Some(self.0.clone());
            model.compat =
                Some(json!({"supportsMidConvoEffort":true,"supportsMidConvoSystemMessages":true}));
            context.register_model(model)?;
        }
        Ok(())
    }
}

fn runtime(base: &str) -> PiRuntime {
    PiRuntime::builder()
        .agent_options(AgentOptions {
            provider_id: "history-fixture".into(),
            model_id: "model".into(),
            thinking_level: ThinkingLevel::High,
            ..Default::default()
        })
        .provider_plugin(FixtureProvider(base.into()))
        .build()
        .unwrap()
}

async fn response(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let number = {
        let mut requests = requests.lock().unwrap();
        requests.push(body);
        requests.len()
    };
    let reason = json!({"type":"reasoning","id":format!("rs_{number}"),"summary":[],"encrypted_content":format!("opaque-{number}")});
    let message = json!({"type":"message","id":format!("msg_{number}"),"phase":"final_answer","role":"assistant","content":[{"type":"output_text","text":"answer","annotations":[]}]});
    let events = [
        json!({"type":"response.output_item.added","output_index":0,"item":reason}),
        json!({"type":"response.output_item.done","output_index":0,"item":reason}),
        json!({"type":"response.output_item.added","output_index":1,"item":message}),
        json!({"type":"response.output_item.done","output_index":1,"item":message}),
        json!({"type":"response.completed","response":{"status":"completed","id":format!("response-{number}")}}),
    ];
    (
        [("content-type", "text/event-stream")],
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect(),
    )
}

#[tokio::test]
async fn resume_preserves_system_positions_effort_and_signed_output_without_provider_work_on_open()
{
    let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/responses", post(response))
        .with_state(requests.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.jsonl");
    let session = AgentSession::create(runtime(&base), &path, AgentSessionOptions::default())
        .await
        .unwrap();
    session
        .run_messages(vec![
            Message::System(SystemMessage::text("base instruction", 1)),
            Message::User(UserMessage::text("one", 2)),
        ])
        .await
        .unwrap();
    session.shutdown().await;
    drop(session);

    let session = AgentSession::open(runtime(&base), &path, AgentSessionOptions::default())
        .await
        .unwrap();
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "opening history must not request the provider"
    );
    let snapshot = session.snapshot();
    let assistant = snapshot
        .agent
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Assistant(message) => Some(message),
            _ => None,
        })
        .unwrap();
    assert_eq!(assistant.provider_thinking_level.as_deref(), Some("high"));
    let encoded = serde_json::to_value(assistant).unwrap();
    let signature: Value =
        serde_json::from_str(encoded["content"][1]["textSignature"].as_str().unwrap()).unwrap();
    assert_eq!(
        signature,
        json!({"v":1,"id":"msg_1","phase":"final_answer"})
    );
    session.set_thinking_level(ThinkingLevel::Low).unwrap();
    session
        .run_messages(vec![
            Message::System(SystemMessage::text("new instruction", 3)),
            Message::User(UserMessage::text("two", 4)),
        ])
        .await
        .unwrap();
    session
        .set_model("history-fixture".into(), "other".into())
        .unwrap();
    session.prompt("three").await.unwrap();
    session.shutdown().await;
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let first = requests[0]["input"].as_array().unwrap();
    let second = requests[1]["input"].as_array().unwrap();
    assert_eq!(&second[..first.len()], first);
    assert!(
        second
            .iter()
            .any(|item| item["type"] == "reasoning" && item["encrypted_content"] == "opaque-1")
    );
    assert_eq!(
        second[second.len() - 3],
        json!({"role":"developer","content":"new instruction"})
    );
    assert_eq!(
        second.last().unwrap(),
        &json!({"type":"configuration_update","reasoning":{"effort":"low"}})
    );
    assert_eq!(requests[1]["reasoning"]["effort"], "medium");
    let third = requests[2]["input"].as_array().unwrap();
    assert!(!third.iter().any(|item| item["type"] == "reasoning"));
    assert_eq!(
        third
            .iter()
            .filter(|item| item["type"] == "configuration_update")
            .count(),
        1
    );
    server.abort();
}
