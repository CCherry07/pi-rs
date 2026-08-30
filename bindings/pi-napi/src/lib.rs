#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use napi::Status;
use napi::bindgen_prelude::{FnArgs, Promise};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use pi_core::{ToolUpdate, ToolUpdateSink};
use pi_js_plugin::{
    ExtensionContextNotification, ExtensionContextQuery, ExtensionContextRequest,
    JsCallbackDispatcher, JsCallbackError, JsGenerationManifest, JsGenerationRequest,
    JsHookBatchInvocation, JsHookBatchResult, JsHostOperation, JsInvocation, JsPluginHost,
    JsStreamHookBatchInvocation, PluginContextHandle, execute_context_notification,
    execute_context_query, execute_context_request,
};
use serde_json::Value;

// A weak TSFN lets Node exit after `runPi()` resolves. The pending `runPi`
// promise itself keeps the process alive while Rust still needs callbacks.
type DispatchArguments = FnArgs<(String, Option<NativeExtensionContext>)>;
type DispatchFunction =
    ThreadsafeFunction<DispatchArguments, Promise<String>, DispatchArguments, Status, false, true>;

/// Generation-scoped native capability injected into a JavaScript callback.
/// A successful session-replacement request explicitly advances this object
/// to the replacement generation. It is intentionally not constructible from
/// JavaScript.
#[napi]
pub struct NativeExtensionContext {
    handle: RwLock<PluginContextHandle>,
    updates: Option<ToolUpdateSink>,
}

impl NativeExtensionContext {
    fn current_handle(&self) -> PluginContextHandle {
        self.handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn replace_handle(&self, handle: PluginContextHandle) {
        *self
            .handle
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = handle;
    }
}

#[napi]
impl NativeExtensionContext {
    #[napi]
    pub fn query(&self, operation: String) -> napi::Result<String> {
        let operation = decode_context_operation::<ExtensionContextQuery>(&operation, "query")?;
        let result =
            execute_context_query(&self.current_handle(), operation).map_err(context_error)?;
        encode_context_result(result)
    }

    #[napi]
    pub fn notify(&self, operation: String) -> napi::Result<()> {
        let operation =
            decode_context_operation::<ExtensionContextNotification>(&operation, "notification")?;
        execute_context_notification(&self.current_handle(), operation).map_err(context_error)
    }

    #[napi]
    pub async fn request(&self, operation: String) -> napi::Result<String> {
        let operation = decode_context_operation::<ExtensionContextRequest>(&operation, "request")?;
        let result = execute_context_request(&self.current_handle(), operation)
            .await
            .map_err(context_error)?;
        if let Some(replacement) = result.replacement {
            self.replace_handle(replacement);
        }
        encode_context_result(result.value)
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

fn context_error(error: pi_js_plugin::PluginContextError) -> napi::Error {
    use pi_js_plugin::PluginContextError;

    let message = match error {
        PluginContextError::Retired => "extension context has retired".to_string(),
        PluginContextError::Unbound => "extension context is not bound to a session".to_string(),
        PluginContextError::CommandOnly(operation) => {
            format!("{operation} is only available in an extension command context")
        }
        PluginContextError::Unavailable(message) => {
            format!("extension context capability is unavailable: {message}")
        }
        PluginContextError::Invalid(message) => {
            format!("invalid extension context operation: {message}")
        }
        PluginContextError::Failed(message) => {
            format!("extension context operation failed: {message}")
        }
    };
    napi::Error::new(Status::GenericFailure, message)
}

struct NativePiHost {
    dispatch: Arc<DispatchFunction>,
    encode_buffer: Mutex<Vec<u8>>,
}

impl NativePiHost {
    fn encode(&self, operation: &JsHostOperation) -> Result<String, JsCallbackError> {
        let mut buffer = self
            .encode_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        buffer.clear();
        serde_json::to_writer(&mut *buffer, operation).map_err(|error| {
            JsCallbackError::new(format!("cannot encode host operation: {error}"))
        })?;
        let encoded = String::from_utf8(buffer.clone()).map_err(|error| {
            JsCallbackError::new(format!("host operation was not valid UTF-8: {error}"))
        })?;
        if buffer.capacity() > 1024 * 1024 {
            *buffer = Vec::with_capacity(2 * 1024);
        }
        Ok(encoded)
    }

    async fn request(
        &self,
        operation: JsHostOperation,
        context: Option<PluginContextHandle>,
        updates: Option<ToolUpdateSink>,
    ) -> Result<Value, JsCallbackError> {
        let operation = self.encode(&operation)?;
        let promise = self
            .dispatch
            .call_async(FnArgs::from((
                operation,
                context.map(|handle| NativeExtensionContext {
                    handle: RwLock::new(handle),
                    updates,
                }),
            )))
            .await
            .map_err(napi_callback_error)?;
        let response = promise.await.map_err(napi_callback_error)?;
        serde_json::from_str(&response).map_err(|error| {
            JsCallbackError::new(format!("JavaScript host returned invalid JSON: {error}"))
        })
    }

    fn notify(&self, operation: JsHostOperation) {
        let Ok(operation) = self.encode(&operation) else {
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
        context: PluginContextHandle,
    ) -> Result<Value, JsCallbackError> {
        self.request(JsHostOperation::Invoke { invocation }, Some(context), None)
            .await
    }

    async fn invoke_with_tool_updates(
        &self,
        invocation: JsInvocation,
        context: PluginContextHandle,
        updates: ToolUpdateSink,
    ) -> Result<Value, JsCallbackError> {
        self.request(
            JsHostOperation::Invoke { invocation },
            Some(context),
            Some(updates),
        )
        .await
    }

    async fn invoke_hook_batch(
        &self,
        invocation: JsHookBatchInvocation,
        context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        let response = self
            .request(
                JsHostOperation::InvokeHookBatch { invocation },
                Some(context),
                None,
            )
            .await?;
        serde_json::from_value(response).map_err(|error| {
            JsCallbackError::new(format!(
                "JavaScript host returned an invalid hook batch result: {error}"
            ))
        })
    }

    async fn invoke_stream_hook_batch(
        &self,
        invocation: JsStreamHookBatchInvocation,
        context: PluginContextHandle,
    ) -> Result<JsHookBatchResult, JsCallbackError> {
        let response = self
            .request(
                JsHostOperation::InvokeStreamHookBatch { invocation },
                Some(context),
                None,
            )
            .await?;
        serde_json::from_value(response).map_err(|error| {
            JsCallbackError::new(format!(
                "JavaScript host returned an invalid stream hook batch result: {error}"
            ))
        })
    }

    fn cancel(&self, invocation_id: &str) {
        self.notify(JsHostOperation::Cancel {
            invocation_id: invocation_id.to_string(),
        });
    }

    fn release_stream(&self, generation_id: &str, stream_id: &str) {
        self.notify(JsHostOperation::ReleaseStream {
            generation_id: generation_id.to_string(),
            stream_id: stream_id.to_string(),
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
        encode_buffer: Mutex::new(Vec::with_capacity(2 * 1024)),
    });
    pi_cli::run_with_js_host(arguments, host)
        .await
        .map_err(|message| napi::Error::new(Status::GenericFailure, message))
}
