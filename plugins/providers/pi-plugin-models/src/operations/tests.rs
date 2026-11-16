use super::*;
use async_trait::async_trait;
use pi_core::{AbortHandle, AnyModelSpec, ModelType};
use pi_plugin::{ProviderAvailability, RegistriesBuilder};
use pi_provider::{
    CredentialKind, CredentialSource, HttpResponse, ProviderCredential, TransportError,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct RotatingSource {
    value: Mutex<ProviderCredential>,
    calls: AtomicUsize,
}
#[async_trait]
impl CredentialSource for RotatingSource {
    async fn resolve(&self, _: &AbortSignal) -> Result<Option<ProviderCredential>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.value.lock().unwrap().clone()))
    }
}
type CapturedRequest = (String, BTreeMap<String, String>, Value);

#[derive(Default)]
struct Transport {
    calls: Mutex<Vec<CapturedRequest>>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn post_json(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &Value,
        _: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        self.calls
            .lock()
            .unwrap()
            .push((url.into(), headers.clone(), body.clone()));
        let body = json!({"answers":{},"usage":{"input_tokens":100,"output_tokens":20}})
            .to_string()
            .into_bytes();
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            headers: vec![],
            body: Box::pin(futures::stream::once(async move { Ok(body) })),
        })
    }
}
fn credential(secret: &str, kind: CredentialKind) -> ProviderCredential {
    ProviderCredential {
        secret: secret.into(),
        kind,
        metadata: BTreeMap::new(),
    }
}

#[tokio::test]
async fn specialized_auth_header_resolves_source_per_request_and_preserves_billed_failures() {
    let directory = tempfile::tempdir().unwrap();
    let source = Arc::new(RotatingSource {
        value: Mutex::new(credential("first", CredentialKind::ApiKey)),
        calls: AtomicUsize::new(0),
    });
    let transport = Arc::new(Transport::default());
    std::fs::write(directory.path().join("models.json"), json!({"providers":{"typesafe":{"authHeader":true,"headers":{"Authorization":"provider-header-must-not-win"}}}}).to_string()).unwrap();
    let options=crate::ModelsPluginOptions::new(directory.path().join("models.json"))
        .base_provider_config("typesafe",json!({"baseUrl":"http://fixture","api":"typesafe-system-one","models":[{"type":"classifier","id":"judge","cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}]}))
        .credential_source("typesafe",source.clone(),true);
    let plugin = crate::ModelsPlugin::load_with_transport(options, transport.clone()).unwrap();
    let (_, _, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![], vec![Arc::new(plugin)])
        .unwrap();
    let runtime = registries.model_runtime();
    assert!(runtime.models().is_empty());
    let models = runtime.available_models_of_type(ModelType::Classifier, None);
    assert_eq!(models.len(), 1);
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    let AnyModelSpec::Classifier(model) = models[0].clone() else {
        panic!()
    };
    let request: ClassifierContext=serde_json::from_value(json!({"state":{},"questions":{"q":{"type":"bool","instructions":"approve?","criteria":{"true":"yes","false":"no"}}}})).unwrap();
    let provider = registries.provider(&model.provider).unwrap();
    for secret in ["first", "second"] {
        source.value.lock().unwrap().secret = secret.into();
        let result = provider
            .classify(
                model.clone(),
                request.clone(),
                ProviderCallContext::without_plugins("/", model.provider.clone(), model.id.clone()),
                AbortHandle::new().1,
            )
            .await
            .unwrap();
        assert_eq!(result.stop_reason, ModelCallStopReason::Error);
        assert!(
            result
                .error_message
                .unwrap()
                .contains("did not return an answer")
        );
        assert_eq!(result.usage.unwrap().total_tokens, 120);
        assert_eq!(
            transport
                .calls
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .get("Authorization"),
            Some(&format!("Bearer {secret}"))
        );
    }
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn openai_decisions_does_not_send_oauth_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let source = Arc::new(RotatingSource {
        value: Mutex::new(credential("oauth-token", CredentialKind::OAuth)),
        calls: AtomicUsize::new(0),
    });
    let transport = Arc::new(Transport::default());
    let options=crate::ModelsPluginOptions::new(directory.path().join("models.json"))
        .base_provider_config("openai",json!({"baseUrl":"http://fixture","api":"openai-decisions","models":[{"type":"classifier","id":"judge"}]}))
        .credential_source("openai",source,false);
    let plugin = crate::ModelsPlugin::load_with_transport(options, transport.clone()).unwrap();
    let (_, _, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![], vec![Arc::new(plugin)])
        .unwrap();
    let model = registries
        .model_runtime()
        .model_of_type(ModelType::Classifier, &"openai".into(), &"judge".into())
        .unwrap();
    let provider = registries.provider(&"openai".into()).unwrap();
    assert_eq!(
        provider.model_availability(&model),
        ProviderAvailability::MissingCredentials
    );
    let AnyModelSpec::Classifier(model) = model else {
        panic!()
    };
    let request: ClassifierContext=serde_json::from_value(json!({"state":{},"questions":{"q":{"type":"bool","instructions":"approve?","criteria":{"true":"yes","false":"no"}}}})).unwrap();
    let result = provider
        .classify(
            model,
            request,
            ProviderCallContext::without_plugins("/", "openai".into(), "judge".into()),
            AbortHandle::new().1,
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("OAuth"));
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[derive(Default)]
struct LlamaTransport {
    headers: Mutex<Vec<BTreeMap<String, String>>>,
}

#[async_trait]
impl HttpTransport for LlamaTransport {
    async fn post_json(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &Value,
        _: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        self.headers.lock().unwrap().push(headers.clone());
        let response = if url.ends_with("/tokenize") {
            json!({"tokens":body["content"].as_str().unwrap().chars().map(|c| c as u32).collect::<Vec<_>>()})
        } else if url.ends_with("/apply-template") {
            json!({"prompt":"Judge the state"})
        } else {
            assert!(url.ends_with("/completion"));
            json!({"completion_probabilities":[{"top_logprobs":[{"id":65,"logprob":-0.1},{"id":66,"logprob":-2.0}]}]})
        };
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            headers: vec![],
            body: Box::pin(futures::stream::once(async move {
                Ok(response.to_string().into_bytes())
            })),
        })
    }
}

#[tokio::test]
async fn llama_catalog_and_requests_allow_optional_authentication() {
    for key in [None, Some("local-key")] {
        let directory = tempfile::tempdir().unwrap();
        let mut options=crate::ModelsPluginOptions::new(directory.path().join("models.json"))
            .base_provider_config("local",json!({"baseUrl":"http://127.0.0.1:8080/v1","api":"llama-cpp-classify","models":[{"type":"classifier","id":"unauthenticated-local"}]}));
        if let Some(key) = key {
            options = options.runtime_api_key("local", key);
        }
        let transport = Arc::new(LlamaTransport::default());
        let plugin = crate::ModelsPlugin::load_with_transport(options, transport.clone()).unwrap();
        let (_, _, registries) = RegistriesBuilder::new()
            .register_plugin_sets(vec![], vec![Arc::new(plugin)])
            .unwrap();
        let available = registries
            .model_runtime()
            .available_models_of_type(ModelType::Classifier, Some(&"local".into()));
        assert_eq!(available.len(), 1);
        let AnyModelSpec::Classifier(model) = available[0].clone() else {
            panic!()
        };
        let request=serde_json::from_value(json!({"state":{},"questions":{"q":{"type":"choice","instructions":"Choose","criteria":{"accept":"yes","reject":"no"}}}})).unwrap();
        let result = registries
            .provider(&model.provider)
            .unwrap()
            .classify(
                model,
                request,
                ProviderCallContext::without_plugins(
                    "/",
                    "local".into(),
                    "unauthenticated-local".into(),
                ),
                AbortHandle::new().1,
            )
            .await
            .unwrap();
        assert_eq!(
            result.stop_reason,
            ModelCallStopReason::Stop,
            "{:?}",
            result.error_message
        );
        assert!(
            matches!(&result.answers["q"],pi_core::ClassifierAnswer::Choice {choice,..} if choice=="accept")
        );
        let captured = transport.headers.lock().unwrap();
        assert!(!captured.is_empty());
        for headers in captured.iter() {
            assert_eq!(
                headers.get("Authorization"),
                key.map(|key| format!("Bearer {key}")).as_ref()
            );
        }
    }
}
