use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use pi_core::{AbortHandle, AbortSignal, AnyModelSpec, ModelCallStopReason, ModelType};
use pi_plugin::{ProviderCallContext, ProviderError, RegistriesBuilder};
use pi_plugin_models::{ModelsPlugin, ModelsPluginOptions};
use pi_provider::{
    CredentialKind, CredentialSource, HttpResponse, HttpTransport, ProviderCredential,
    TransportError,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Source {
    value: Mutex<Option<ProviderCredential>>,
    checks: AtomicUsize,
    resolutions: AtomicUsize,
}

#[async_trait]
impl CredentialSource for Source {
    async fn check(&self, _: &AbortSignal) -> Result<Option<ProviderCredential>, ProviderError> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(self.value.lock().unwrap().clone())
    }
    async fn resolve(&self, _: &AbortSignal) -> Result<Option<ProviderCredential>, ProviderError> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        Ok(self.value.lock().unwrap().clone())
    }
}

type Captured = (String, BTreeMap<String, String>, Value);
#[derive(Default)]
struct Transport(Mutex<Vec<Captured>>);
#[async_trait]
impl HttpTransport for Transport {
    async fn post_json(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &Value,
        _: AbortSignal,
    ) -> Result<HttpResponse, TransportError> {
        self.0
            .lock()
            .unwrap()
            .push((url.into(), headers.clone(), body.clone()));
        let response =
            json!({"success":true,"result":{"answers":{"q":{"type":"noul","noul":0.75}}}})
                .to_string()
                .into_bytes();
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            headers: vec![],
            body: Box::pin(futures::stream::once(async move { Ok(response) })),
        })
    }
}

#[tokio::test]
async fn cloudflare_queries_and_requests_use_current_account_without_mutating_the_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let source = Arc::new(Source::default());
    let transport = Arc::new(Transport::default());
    let plugin = ModelsPlugin::load_with_transport(
        ModelsPluginOptions::new(directory.path().join("models.json"))
            .with_builtin_specialized_models()
            .credential_source("cloudflare-workers-ai", source.clone(), false),
        transport.clone(),
    )
    .unwrap();
    let (_, _, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![], vec![Arc::new(plugin)])
        .unwrap();
    let runtime = registries.model_runtime();
    let provider_id = "cloudflare-workers-ai".into();
    let registered = runtime.models_of_type(ModelType::Classifier, Some(&provider_id));
    assert_eq!(registered.len(), 3);
    assert!(
        runtime
            .query_available_models_of_type(
                ModelType::Classifier,
                Some(&provider_id),
                AbortHandle::new().1
            )
            .await
            .unwrap()
            .is_empty()
    );
    for (key, account) in [
        ("first-key", ""),
        ("first-key", "account-one"),
        ("second-key", "account-two"),
    ] {
        *source.value.lock().unwrap() = Some(ProviderCredential {
            secret: key.into(),
            kind: CredentialKind::ApiKey,
            metadata: BTreeMap::from([("env".into(), json!({"CLOUDFLARE_ACCOUNT_ID":account}))]),
        });
        let available = runtime
            .query_available_models_of_type(
                ModelType::Classifier,
                Some(&provider_id),
                AbortHandle::new().1,
            )
            .await
            .unwrap();
        if account.is_empty() {
            assert!(available.is_empty());
            continue;
        }
        assert_eq!(available, registered);
        let AnyModelSpec::Classifier(model) = available[0].clone() else {
            panic!("classifier")
        };
        let model_id = model.id.clone();
        let input = serde_json::from_value(json!({"state":{"text":"fixture"},"questions":{"q":{"type":"bool","instructions":"Approved?","criteria":{"true":"yes","false":"no"}}}})).unwrap();
        let result = registries
            .provider(&provider_id)
            .unwrap()
            .classify(
                model,
                input,
                ProviderCallContext::without_plugins("/", provider_id.clone(), model_id.clone()),
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
        let captured = transport.0.lock().unwrap();
        let (url, headers, body) = captured.last().unwrap();
        assert_eq!(
            url,
            &format!("https://api.cloudflare.com/client/v4/accounts/{account}/ai/run")
        );
        assert_eq!(headers.get("Authorization"), Some(&format!("Bearer {key}")));
        assert_eq!(body["model"], model_id.as_str());
        assert_eq!(body["input"]["state"]["text"], "fixture");
    }
    assert_eq!(
        runtime.models_of_type(ModelType::Classifier, Some(&provider_id)),
        registered
    );
    assert_eq!(source.checks.load(Ordering::SeqCst), 4);
    assert_eq!(source.resolutions.load(Ordering::SeqCst), 2);
    assert_eq!(transport.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn queries_check_templates_without_executing_credential_commands() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("must-not-exist");
    let options = ModelsPluginOptions::new(directory.path().join("models.json"))
        .base_provider_config("command", json!({"api":"typesafe-system-one","baseUrl":"http://fixture.invalid","apiKey":format!("!touch '{}'; printf secret", marker.display()),"models":[{"type":"classifier","id":"judge","headers":{"Authorization":"Bearer request-only-test-secret","X-Command":format!("!touch {}",marker.display())}}]}))
        .base_provider_config("missing-env", json!({"api":"typesafe-system-one","baseUrl":"http://fixture.invalid","apiKey":"$PI_CODEMODE_AVAILABILITY_TEST_UNSET_826147","models":[{"type":"classifier","id":"judge"}]}));
    let transport = Arc::new(Transport::default());
    let plugin = ModelsPlugin::load_with_transport(options, transport.clone()).unwrap();
    let (_, _, registries) = RegistriesBuilder::new()
        .register_plugin_sets(vec![], vec![Arc::new(plugin)])
        .unwrap();
    let models = registries
        .model_runtime()
        .query_available_models_of_type(ModelType::Classifier, None, AbortHandle::new().1)
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].provider().as_str(), "command");
    let catalog = serde_json::to_value(&models).unwrap();
    assert!(catalog[0].get("headers").is_none());
    assert!(!catalog.to_string().contains("request-only-test-secret"));
    assert!(!marker.exists());
    assert!(transport.0.lock().unwrap().is_empty());
}
