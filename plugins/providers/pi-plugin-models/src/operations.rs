//! Specialized HTTP model protocols. Routing and credentials remain provider-owned.
mod decisions;
mod images;
mod llama;
mod system_one;

use std::collections::BTreeMap;

use pi_core::{
    AbortSignal, ClassifierContext, ClassifierModelSpec, ClassifierResult, ImageModelSpec,
    ImagesContext, ImagesResult, ModelCallStopReason, ModelCost, Usage,
};
use pi_plugin::{ProviderCallContext, ProviderError};
use pi_provider::{
    HttpTransport, collect_body_limited, insert_header, post_json_with_provider_hooks,
    read_error_response,
};
use serde_json::Value;

pub(crate) struct OperationClient<'a> {
    pub transport: &'a dyn HttpTransport,
    pub context: &'a ProviderCallContext,
    pub headers: BTreeMap<String, String>,
    pub signal: AbortSignal,
}

impl OperationClient<'_> {
    async fn post(
        &self,
        url: String,
        payload: Value,
        observe: bool,
    ) -> Result<Value, ProviderError> {
        let result = async {
            let payload = if observe {
                self.context
                    .before_provider_request(&self.signal, payload)
                    .await?
            } else {
                payload
            };
            let response = if observe {
                post_json_with_provider_hooks(
                    self.transport,
                    self.context,
                    &url,
                    self.headers.clone(),
                    &payload,
                    self.signal.clone(),
                )
                .await
            } else {
                self.transport
                    .post_json(&url, &self.headers, &payload, self.signal.clone())
                    .await
            }
            .map_err(|error| error.into_provider_error())?;
            if !(200..300).contains(&response.status) {
                return Err(ProviderError::Failure(
                    read_error_response(response)
                        .await
                        .map_err(|e| e.into_provider_error())?,
                ));
            }
            let body = collect_body_limited(response.body, 32 * 1024 * 1024)
                .await
                .map_err(|e| e.into_provider_error())?;
            serde_json::from_str(&body)
                .map_err(|e| ProviderError::Protocol(format!("invalid model response: {e}")))
        };
        tokio::select! { biased; _ = self.signal.wait() => Err(ProviderError::Aborted), result = result => result }
    }
}

pub(crate) async fn classify(
    model: ClassifierModelSpec,
    input: ClassifierContext,
    mut client: OperationClient<'_>,
) -> ClassifierResult {
    let mut output = ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: BTreeMap::new(),
        usage: None,
        stop_reason: ModelCallStopReason::Stop,
        error_message: None,
        timestamp_ms: now_ms(),
    };
    let call = async {
        let base = model
            .base_url
            .as_deref()
            .ok_or_else(|| invalid("classifier model requires baseUrl"))?
            .trim_end_matches('/');
        if model.api == "llama-cpp-classify" {
            return llama::classify(&model, &input, |url, payload, observe| {
                Box::pin(client.post(url, payload, observe))
            })
            .await;
        }
        let (url, payload) = match model.api.as_str() {
            "typesafe-system-one" | "cloudflare-workers-ai-system-one" => {
                let (payload, opt_in) = system_one::payload(&model, &input)?;
                if opt_in
                    && !client
                        .headers
                        .keys()
                        .any(|key| key.eq_ignore_ascii_case("TypeSafe-Danger-Zone-Images"))
                {
                    insert_header(&mut client.headers, "TypeSafe-Danger-Zone-Images", "true");
                }
                let endpoint = if model.api == "typesafe-system-one" {
                    "systemone"
                } else {
                    "run"
                };
                (format!("{base}/{endpoint}"), payload)
            }
            "openai-decisions" => (
                format!("{base}/decisions"),
                decisions::payload(model.id.as_str(), &input)?,
            ),
            api => return Err(invalid(format!("unsupported classifier API: {api}"))),
        };
        let response = client.post(url, payload, true).await?;
        let body = if model.api == "cloudflare-workers-ai-system-one" {
            system_one::cloudflare_output(&response)?
        } else {
            &response
        };
        // Billed usage survives malformed answers, as it does in Pi.
        output.usage = classifier_usage(body.get("usage"), &model.cost);
        if model.api == "openai-decisions" {
            decisions::answers(body, &input)
        } else {
            system_one::answers(body, &input)
        }
    }
    .await;
    match call {
        Ok(answers) => output.answers = answers,
        Err(error) => {
            output.stop_reason = if client.signal.is_aborted() {
                ModelCallStopReason::Aborted
            } else {
                ModelCallStopReason::Error
            };
            output.error_message = Some(error.to_string());
        }
    }
    output
}

pub(crate) async fn generate_images(
    model: ImageModelSpec,
    input: ImagesContext,
    client: OperationClient<'_>,
) -> ImagesResult {
    let mut output = ImagesResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: vec![],
        response_id: None,
        usage: None,
        stop_reason: ModelCallStopReason::Stop,
        error_message: None,
        timestamp_ms: now_ms(),
    };
    let call = async {
        if model.api != "openrouter-images" {
            return Err(invalid(format!("unsupported image API: {}", model.api)));
        }
        let base = model
            .base_url
            .as_deref()
            .ok_or_else(|| invalid("image model requires baseUrl"))?
            .trim_end_matches('/');
        let response = client
            .post(
                format!("{base}/chat/completions"),
                images::payload(&model, &input),
                true,
            )
            .await?;
        output.response_id = response
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        output.usage = images::usage(response.get("usage"), &model.cost);
        output.output = images::content(&response);
        Ok::<_, ProviderError>(())
    }
    .await;
    if let Err(error) = call {
        output.stop_reason = if client.signal.is_aborted() {
            ModelCallStopReason::Aborted
        } else {
            ModelCallStopReason::Error
        };
        output.error_message = Some(error.to_string());
    }
    output
}

fn classifier_usage(value: Option<&Value>, cost: &ModelCost) -> Option<Usage> {
    let value = value?.as_object()?;
    if !value.contains_key("input_tokens") && !value.contains_key("output_tokens") {
        return None;
    }
    let input = count(value.get("input_tokens"));
    let output = count(value.get("output_tokens"));
    let mut usage = Usage {
        input,
        output,
        total_tokens: input.saturating_add(output),
        ..Usage::default()
    };
    usage.cost = cost.calculate(&usage);
    Some(usage)
}
fn count(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}
fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::Protocol(message.into())
}
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests;
