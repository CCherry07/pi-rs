#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::Arc;

use async_trait::async_trait;
use napi::Status;
use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use pi_js_plugin::{
    JsCallbackDispatcher, JsCallbackError, JsGenerationManifest, JsGenerationRequest,
    JsHostOperation, JsInvocation, JsPluginHost,
};
use serde_json::Value;

// A weak TSFN lets Node exit after `runPi()` resolves. The pending `runPi`
// promise itself keeps the process alive while Rust still needs callbacks.
type DispatchFunction = ThreadsafeFunction<String, Promise<String>, String, Status, false, true>;

struct NapiJsPluginHost {
    dispatch: Arc<DispatchFunction>,
}

impl NapiJsPluginHost {
    async fn request(&self, operation: JsHostOperation) -> Result<Value, JsCallbackError> {
        let operation = serde_json::to_string(&operation).map_err(|error| {
            JsCallbackError::new(format!("cannot encode host operation: {error}"))
        })?;
        let promise = self
            .dispatch
            .call_async(operation)
            .await
            .map_err(napi_callback_error)?;
        let response = promise.await.map_err(napi_callback_error)?;
        serde_json::from_str(&response).map_err(|error| {
            JsCallbackError::new(format!("JavaScript host returned invalid JSON: {error}"))
        })
    }

    fn notify(&self, operation: JsHostOperation) {
        let Ok(operation) = serde_json::to_string(&operation) else {
            return;
        };
        let _ = self
            .dispatch
            .call(operation, ThreadsafeFunctionCallMode::NonBlocking);
    }
}

fn napi_callback_error(error: napi::Error) -> JsCallbackError {
    JsCallbackError::new(error.to_string())
}

#[async_trait]
impl JsCallbackDispatcher for NapiJsPluginHost {
    async fn invoke(&self, invocation: JsInvocation) -> Result<Value, JsCallbackError> {
        self.request(JsHostOperation::Invoke { invocation }).await
    }

    fn cancel(&self, invocation_id: &str) {
        self.notify(JsHostOperation::Cancel {
            invocation_id: invocation_id.to_string(),
        });
    }

    fn retire_generation(&self, generation_id: &str) {
        self.notify(JsHostOperation::RetireGeneration {
            generation_id: generation_id.to_string(),
        });
    }
}

#[async_trait]
impl JsPluginHost for NapiJsPluginHost {
    async fn prepare_generation(
        &self,
        request: JsGenerationRequest,
    ) -> Result<JsGenerationManifest, JsCallbackError> {
        let response = self
            .request(JsHostOperation::PrepareGeneration { request })
            .await?;
        serde_json::from_value(response).map_err(|error| {
            JsCallbackError::new(format!(
                "JavaScript host returned an invalid manifest: {error}"
            ))
        })
    }
}

/// Starts the Rust CLI/TUI while Node owns JavaScript extension loading and
/// callback execution. `arguments` matches `process.argv.slice(2)`.
#[napi]
pub async fn run_pi(arguments: Vec<String>, dispatch: DispatchFunction) -> napi::Result<()> {
    dotenvy::dotenv().ok();
    let host: Arc<dyn JsPluginHost> = Arc::new(NapiJsPluginHost {
        dispatch: Arc::new(dispatch),
    });
    pi_cli::run_with_js_host(arguments, host)
        .await
        .map_err(|message| napi::Error::new(Status::GenericFailure, message))
}
