//! Verify actual request-body defaults through the public provider/HTTP transport seam.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortHandle, AbortSignal, ThinkingLevel};
use pi_plugin::{ProviderCallContext, ProviderRequest, RegistriesBuilder};
use pi_plugin_models::{ModelsPlugin, ModelsPluginOptions};
use pi_provider::{HttpResponse, HttpTransport, TransportError};
use serde_json::{Value, json};

#[derive(Default)]
struct Capture(Mutex<Vec<Value>>);
#[async_trait]
impl HttpTransport for Capture {
    async fn post_json(
        &self,
        _: &str,
        _: &BTreeMap<String, String>,
        body: &Value,
        _: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        self.0.lock().unwrap().push(body.clone());
        Ok(HttpResponse {
            status: 200,
            content_type: Some("text/event-stream".into()),
            headers: vec![],
            body: Box::pin(futures::stream::empty()),
        })
    }
}

async fn body(reasoning: bool, level: ThinkingLevel, explicit: Value) -> Value {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("models.json");
    std::fs::write(&path, json!({"providers":{"sampling-fixture":{
        "api":"openai-completions", "baseUrl":"http://127.0.0.1:9", "apiKey":"fixture-key",
        "models":[{
            "id":"model", "reasoning":reasoning,
            "thinkingLevelMap":{"high":"medium"},
            "samplingParams":{"temperature":0.1,"baseOnly":"keep","shared":{"base":true},"empty":"fallback"},
            "samplingParamsByThinkingLevel":{
                "off":{"temperature":0,"offOnly":true},
                "high":{"temperature":0.4,"levelOnly":true,"shared":{"level":true},"empty":"","clear":null,"enabled":false},
                "medium":{"temperature":0.8,"wrongLevel":true}
            }
        }]
    }}}).to_string()).unwrap();
    let capture = Arc::new(Capture::default());
    let plugin =
        ModelsPlugin::load_with_transport(ModelsPluginOptions::new(path), capture.clone()).unwrap();
    let (_, _, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![], vec![Arc::new(plugin)])
        .unwrap();
    let provider = registries.provider(&"sampling-fixture".into()).unwrap();
    let (_, signal) = AbortHandle::new();
    let _stream = provider
        .stream(
            ProviderRequest {
                model: "model".into(),
                model_spec: None,
                system_prompt: "fixture".into(),
                messages: vec![],
                tools: vec![],
                thinking_level: level,
                thinking_budgets: None,
                max_output_tokens: None,
                headers: BTreeMap::new(),
                sampling_params: serde_json::from_value(explicit).unwrap(),
                session_id: None,
            },
            ProviderCallContext::without_plugins(
                directory.path(),
                "sampling-fixture".into(),
                "model".into(),
            ),
            signal,
        )
        .await
        .unwrap();
    let mut requests = capture.0.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "request was not sent through the capturing transport"
    );
    requests.pop().unwrap()
}

#[tokio::test]
async fn pi_level_sampling_precedes_provider_mapping_and_explicit_request_fields_win() {
    let level = body(true, ThinkingLevel::High, json!({})).await;
    assert_eq!(level["temperature"], 0.4);
    assert_eq!(level["reasoning_effort"], "medium");
    assert_eq!(level["baseOnly"], "keep");
    assert_eq!(level["levelOnly"], true);
    assert_eq!(level["shared"], json!({"level":true}));
    assert_eq!(level["empty"], "");
    assert_eq!(level["enabled"], false);
    assert_eq!(level.get("clear"), Some(&Value::Null));
    assert!(level.get("wrongLevel").is_none());

    let explicit = body(
        true,
        ThinkingLevel::High,
        json!({
            "temperature":0.9,"requestOnly":true,"shared":{"request":true}
        }),
    )
    .await;
    assert_eq!(explicit["temperature"], 0.9);
    assert_eq!(explicit["requestOnly"], true);
    assert_eq!(explicit["levelOnly"], true);
    assert_eq!(explicit["baseOnly"], "keep");
    assert_eq!(explicit["shared"], json!({"request":true}));
}

#[tokio::test]
async fn sampling_uses_the_clamped_pi_level_for_unmapped_and_nonreasoning_requests() {
    assert_eq!(
        body(true, ThinkingLevel::XHigh, json!({})).await["temperature"],
        0.4
    );
    let disabled = body(false, ThinkingLevel::High, json!({})).await;
    assert_eq!(disabled["temperature"], 0);
    assert_eq!(disabled["offOnly"], true);
    assert!(disabled.get("levelOnly").is_none());
}
