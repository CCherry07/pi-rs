//! Request conformance against legacy/pi/packages/ai/test/openai-responses-*.test.ts.
//! Only the HTTP boundary is replaced; no real credentials or network are used.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{AbortHandle, AbortSignal, ModelSpec, ThinkingLevel};
use pi_plugin::{Provider, ProviderCallContext, ProviderRequest};
use pi_plugin_openai::{OpenAiCompatibleConfig, OpenAiResponsesCompatibleProvider};
use pi_provider::{HttpResponse, HttpTransport, TransportError};
use serde_json::{Value, json};

type CapturedRequest = (String, BTreeMap<String, String>, Value);

#[derive(Default)]
struct Capture(Mutex<Option<CapturedRequest>>);

#[async_trait]
impl HttpTransport for Capture {
    async fn post_json(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &Value,
        _: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        *self.0.lock().unwrap() = Some((url.into(), headers.clone(), body.clone()));
        Ok(HttpResponse {
            status: 200,
            content_type: Some("text/event-stream".into()),
            headers: Vec::new(),
            body: Box::pin(futures::stream::empty()),
        })
    }
}

fn request(provider: &str, base_url: &str) -> ProviderRequest {
    let mut model = ModelSpec::new(provider, "model", "Model", "openai-responses");
    model.base_url = Some(base_url.into());
    ProviderRequest {
        model: model.id.clone(),
        model_spec: Some(model),
        system_prompt: String::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        thinking_level: ThinkingLevel::Off,
        thinking_budgets: None,
        max_output_tokens: Some(1_024),
        headers: BTreeMap::new(),
        sampling_params: BTreeMap::new(),
        session_id: Some("session-123".into()),
    }
}

async fn capture(key: &str, request: ProviderRequest) -> (BTreeMap<String, String>, Value) {
    let model = request.model_spec.as_ref().unwrap();
    let transport = Arc::new(Capture::default());
    let provider = OpenAiResponsesCompatibleProvider::with_transport(
        OpenAiCompatibleConfig::new(model.base_url.as_ref().unwrap(), key)
            .provider_id(model.provider.clone()),
        transport.clone(),
    )
    .unwrap();
    let context = ProviderCallContext::without_plugins("/", provider.id(), request.model.clone());
    let (_, signal) = AbortHandle::new();
    let _stream = provider.stream(request, context, signal).await.unwrap();
    let (url, headers, body) = transport.0.lock().unwrap().take().unwrap();
    assert!(url.ends_with("/responses"));
    (headers, body)
}

/// Cache retention is an environment-backed product setting. Isolate each case
/// in a child test process rather than mutating the parallel test runner's env.
fn cache_mode(test: &str) -> Option<String> {
    const CHILD: &str = "PI_RESPONSES_REQUEST_TEST";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return Some(std::env::var("PI_CACHE_RETENTION").unwrap());
    }
    for mode in ["none", "short", "long"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, test)
            .env("PI_CACHE_RETENTION", mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    None
}

struct PayloadHook;

#[pi_plugin::provider_plugin]
impl pi_plugin::ProviderPlugin for PayloadHook {
    fn id(&self) -> pi_core::PluginId {
        "payload-fixture".into()
    }

    async fn before_provider_request(
        &self,
        _: pi_plugin::ProviderPluginContext,
        event: pi_plugin::BeforeProviderRequestEvent,
    ) -> Result<Option<Value>, pi_plugin::PluginError> {
        assert!(event.payload.get("max_output_tokens").is_none());
        let mut payload = event.payload;
        payload["max_output_tokens"] = json!(77);
        Ok(Some(payload))
    }
}

#[tokio::test]
async fn chatgpt_policy_precedes_explicit_sampling_and_provider_hooks() {
    let mut request = request("openai", "https://api.openai.com/v1");
    // Pi applies free-form samplingParams last, even for ChatGPT tokens. Rust
    // has no separate named temperature option; this is the explicit override.
    request.sampling_params = BTreeMap::from([
        ("max_output_tokens".into(), json!(55)),
        ("temperature".into(), json!(0.5)),
        ("prompt_cache_options".into(), json!({"ttl": "override"})),
    ]);
    let (_, body) = capture("chatgpt-access-token", request.clone()).await;
    assert_eq!(body["max_output_tokens"], 55);
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["prompt_cache_options"], json!({"ttl": "override"}));

    request.sampling_params.clear();
    let transport = Arc::new(Capture::default());
    let provider = OpenAiResponsesCompatibleProvider::with_transport(
        OpenAiCompatibleConfig::new("https://api.openai.com/v1", "chatgpt-access-token")
            .provider_id("openai"),
        transport.clone(),
    )
    .unwrap();
    let context = ProviderCallContext::new(
        1,
        "/",
        "openai".into(),
        request.model.clone(),
        Arc::new(pi_plugin::ProviderPluginDriver::new(vec![Arc::new(PayloadHook)]).unwrap()),
    );
    let (_, signal) = AbortHandle::new();
    let _stream = provider.stream(request, context, signal).await.unwrap();
    assert_eq!(
        transport.0.lock().unwrap().as_ref().unwrap().2["max_output_tokens"],
        77
    );
}

#[tokio::test]
async fn responses_affinity_uses_pi_header_names_and_respects_cache_disable_and_overrides() {
    let Some(mode) = cache_mode(
        "responses_affinity_uses_pi_header_names_and_respects_cache_disable_and_overrides",
    ) else {
        return;
    };
    for format in ["openai", "openai-nosession", "openrouter"] {
        let mut request = request("custom", "https://gateway.example/v1");
        request.model_spec.as_mut().unwrap().compat =
            Some(json!({"sessionAffinityFormat": format}));
        let (headers, _) = capture("gateway-key", request.clone()).await;
        let header = |name: &str| headers.get(name).map(String::as_str);
        assert_eq!(
            header("session-id"),
            (mode != "none" && format == "openai").then_some("session-123")
        );
        assert_eq!(
            header("x-client-request-id"),
            (mode != "none" && format != "openrouter").then_some("session-123")
        );
        assert_eq!(
            header("x-session-id"),
            (mode != "none" && format == "openrouter").then_some("session-123")
        );
        assert!(!headers.contains_key("session_id"));

        request
            .headers
            .insert("Session-ID".into(), "explicit-session".into());
        request
            .headers
            .insert("X-Client-Request-ID".into(), "explicit-request".into());
        let (headers, _) = capture("gateway-key", request).await;
        assert_eq!(headers["Session-ID"], "explicit-session");
        assert_eq!(headers["X-Client-Request-ID"], "explicit-request");
        assert!(!headers.contains_key("session-id"));
    }
}

#[tokio::test]
async fn responses_cache_policy_respects_modes_limits_and_chatgpt_grants() {
    let Some(mode) = cache_mode("responses_cache_policy_respects_modes_limits_and_chatgpt_grants")
    else {
        return;
    };
    for key_mode in ["auto", "enabled", "disabled"] {
        for explicit in [false, true] {
            for long_supported in [false, true] {
                for (key, sign_in) in [("sk-fixture", false), ("chatgpt-access-token", true)] {
                    let mut request = request("openai", "https://api.openai.com/v1");
                    request.session_id = Some("界🙂".repeat(40));
                    request.model_spec.as_mut().unwrap().compat = Some(json!({
                        "promptCacheKeyMode": key_mode,
                        "supportsExplicitPromptCacheMode": explicit,
                        "supportsLongCacheRetention": long_supported
                    }));
                    let (_, body) = capture(key, request).await;
                    assert_eq!(
                        body.get("prompt_cache_key"),
                        (mode != "none" && key_mode != "disabled")
                            .then_some(&json!("界🙂".repeat(32)))
                    );
                    assert_eq!(
                        body.get("prompt_cache_retention"),
                        (!sign_in && mode == "long" && long_supported && !explicit)
                            .then_some(&json!("24h"))
                    );
                    let options = if sign_in || !explicit {
                        None
                    } else if mode == "none" {
                        Some(json!({"mode": "explicit"}))
                    } else if mode == "long" && long_supported {
                        Some(json!({"ttl": "30m"}))
                    } else {
                        None
                    };
                    assert_eq!(body.get("prompt_cache_options"), options.as_ref());
                }
            }
        }
    }
}

#[tokio::test]
async fn responses_compat_can_disable_the_generated_output_budget() {
    for (compat, expected) in [
        (json!({}), Some(16)),
        (json!({"supportsMaxOutputTokens": true}), Some(16)),
        (json!({"supportsMaxOutputTokens": false}), None),
    ] {
        let mut request = request("custom", "https://gateway.example/v1");
        request.max_output_tokens = Some(1);
        request.model_spec.as_mut().unwrap().compat = Some(compat);
        let (_, body) = capture("gateway-key", request).await;
        assert_eq!(
            body.get("max_output_tokens").and_then(Value::as_u64),
            expected
        );
    }
}

#[tokio::test]
async fn chatgpt_sign_in_omits_generated_fields_without_changing_api_keys_or_gateways() {
    let Some(mode) =
        cache_mode("chatgpt_sign_in_omits_generated_fields_without_changing_api_keys_or_gateways")
    else {
        return;
    };
    for (provider, base, key, sign_in) in [
        (
            "openai",
            "https://api.openai.com/v1",
            "chatgpt-access-token",
            true,
        ),
        (
            "openai",
            "https://api.openai.com/v1",
            "sk-proj-fixture",
            false,
        ),
        (
            "openai",
            "https://gateway.example/v1",
            "gateway-token",
            false,
        ),
        ("custom", "https://api.openai.com/v1", "custom-token", false),
    ] {
        let (_, body) = capture(key, request(provider, base)).await;
        assert_eq!(
            body.get("max_output_tokens"),
            (!sign_in).then_some(&json!(1_024))
        );
        assert_eq!(
            body.get("prompt_cache_retention"),
            (!sign_in && mode == "long").then_some(&json!("24h"))
        );
        assert!(body.get("prompt_cache_options").is_none());
        assert_eq!(
            body.get("prompt_cache_key"),
            (mode != "none").then_some(&json!("session-123"))
        );
        assert_eq!(body["store"], false);
    }
}
