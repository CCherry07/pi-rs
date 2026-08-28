#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::Arc;

use async_trait::async_trait;
use napi::Status;
use napi::bindgen_prelude::{FnArgs, Promise};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use pi_core::{ToolUpdate, ToolUpdateSink};
use pi_js_plugin::{
    ExtensionContextHandle, ExtensionContextNotification, ExtensionContextQuery,
    ExtensionContextRequest, JsCallbackDispatcher, JsCallbackError, JsGenerationManifest,
    JsGenerationRequest, JsHostOperation, JsInvocation, JsPluginHost,
};
use serde_json::Value;

// A weak TSFN lets Node exit after `runPi()` resolves. The pending `runPi`
// promise itself keeps the process alive while Rust still needs callbacks.
type DispatchArguments = FnArgs<(String, Option<NativeExtensionContext>)>;
type DispatchFunction =
    ThreadsafeFunction<DispatchArguments, Promise<String>, DispatchArguments, Status, false, true>;

/// Generation-scoped native capability injected into a JavaScript callback.
/// It is intentionally not constructible from JavaScript.
#[napi]
pub struct NativeExtensionContext {
    handle: ExtensionContextHandle,
    updates: Option<ToolUpdateSink>,
}

#[napi]
impl NativeExtensionContext {
    #[napi]
    pub fn query(&self, operation: String) -> napi::Result<String> {
        let operation = decode_context_operation::<ExtensionContextQuery>(&operation, "query")?;
        let result = self.handle.query(operation).map_err(context_error)?;
        encode_context_result(result)
    }

    #[napi]
    pub fn notify(&self, operation: String) -> napi::Result<()> {
        let operation =
            decode_context_operation::<ExtensionContextNotification>(&operation, "notification")?;
        self.handle.notify(operation).map_err(context_error)
    }

    #[napi]
    pub async fn request(&self, operation: String) -> napi::Result<String> {
        let operation = decode_context_operation::<ExtensionContextRequest>(&operation, "request")?;
        let result = self
            .handle
            .clone()
            .request(operation)
            .await
            .map_err(context_error)?;
        encode_context_result(result)
    }

    #[napi]
    pub fn update(&self, result: String) -> napi::Result<()> {
        let update = serde_json::from_str::<ToolUpdate>(&result).map_err(|error| {
            napi::Error::new(
                Status::InvalidArg,
                format!("invalid JavaScript tool update: {error}"),
            )
        })?;
        let updates = self.updates.as_ref().ok_or_else(|| {
            napi::Error::new(
                Status::GenericFailure,
                "tool updates are unavailable for this callback".to_string(),
            )
        })?;
        if updates.send(update) {
            Ok(())
        } else {
            Err(napi::Error::new(
                Status::GenericFailure,
                "tool update receiver is closed".to_string(),
            ))
        }
    }
}

fn decode_context_operation<T: serde::de::DeserializeOwned>(
    operation: &str,
    kind: &str,
) -> napi::Result<T> {
    serde_json::from_str(operation).map_err(|error| {
        napi::Error::new(
            Status::InvalidArg,
            format!("invalid JavaScript extension context {kind}: {error}"),
        )
    })
}

fn encode_context_result(result: Value) -> napi::Result<String> {
    serde_json::to_string(&result).map_err(|error| {
        napi::Error::new(
            Status::GenericFailure,
            format!("cannot encode JavaScript extension context result: {error}"),
        )
    })
}

fn context_error(error: pi_js_plugin::ExtensionContextError) -> napi::Error {
    napi::Error::new(Status::GenericFailure, error.to_string())
}

struct NativePiHost {
    dispatch: Arc<DispatchFunction>,
}

impl NativePiHost {
    async fn request(
        &self,
        operation: JsHostOperation,
        context: Option<ExtensionContextHandle>,
        updates: Option<ToolUpdateSink>,
    ) -> Result<Value, JsCallbackError> {
        let operation = serde_json::to_string(&operation).map_err(|error| {
            JsCallbackError::new(format!("cannot encode host operation: {error}"))
        })?;
        let promise = self
            .dispatch
            .call_async(FnArgs::from((
                operation,
                context.map(|handle| NativeExtensionContext { handle, updates }),
            )))
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
        let _ = self.dispatch.call(
            FnArgs::from((operation, None)),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }
}

fn napi_callback_error(error: napi::Error) -> JsCallbackError {
    JsCallbackError::new(error.to_string())
}

#[async_trait]
impl JsCallbackDispatcher for NativePiHost {
    async fn invoke(
        &self,
        invocation: JsInvocation,
        context: ExtensionContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.request(JsHostOperation::Invoke { invocation }, Some(context), None)
            .await
    }

    async fn invoke_with_tool_updates(
        &self,
        invocation: JsInvocation,
        context: ExtensionContextHandle,
        updates: ToolUpdateSink,
    ) -> Result<Value, JsCallbackError> {
        self.request(
            JsHostOperation::Invoke { invocation },
            Some(context),
            Some(updates),
        )
        .await
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
impl JsPluginHost for NativePiHost {
    async fn prepare_generation(
        &self,
        request: JsGenerationRequest,
    ) -> Result<JsGenerationManifest, JsCallbackError> {
        let response = self
            .request(JsHostOperation::PrepareGeneration { request }, None, None)
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
    let host: Arc<dyn JsPluginHost> = Arc::new(NativePiHost {
        dispatch: Arc::new(dispatch),
    });
    pi_cli::run_with_js_host(arguments, host)
        .await
        .map_err(|message| napi::Error::new(Status::GenericFailure, message))
}
