//! The script's model namespace. Model identity is resolved by the host;
//! arguments can never supply a transport URL, credentials, or headers.

use std::sync::atomic::{AtomicUsize, Ordering};

use pi_core::{
    AbortSignal, ClassifierContext, ImagesContext, ModelId, ModelType, ProviderId, ToolCallId,
    ToolResult, Usage,
};
use pi_plugin::{NestedToolResult, ToolContext, ToolError};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Copy)]
pub(crate) enum ModelOperation {
    List,
    Available,
    Find,
    Classify,
    Images,
}

impl ModelOperation {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "getModelsOfType" => Some(Self::List),
            "getAvailableOfType" => Some(Self::Available),
            "getModelOfType" => Some(Self::Find),
            "classify" => Some(Self::Classify),
            "generateImages" => Some(Self::Images),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::List => "getModelsOfType",
            Self::Available => "getAvailableOfType",
            Self::Find => "getModelOfType",
            Self::Classify => "classify",
            Self::Images => "generateImages",
        }
    }

    pub fn is_inference(self) -> bool {
        matches!(self, Self::Classify | Self::Images)
    }
}

pub(crate) struct ModelCalls {
    inference: Semaphore,
}

impl Default for ModelCalls {
    fn default() -> Self {
        Self {
            inference: Semaphore::new(4),
        }
    }
}

pub(crate) enum ModelRequest {
    List {
        kind: ModelType,
        provider: Option<ProviderId>,
        available: bool,
    },
    Find {
        kind: ModelType,
        provider: ProviderId,
        model_id: ModelId,
    },
    Classifier {
        provider: ProviderId,
        model_id: ModelId,
        input: ClassifierContext,
    },
    Images {
        provider: ProviderId,
        model_id: ModelId,
        input: ImagesContext,
    },
}

impl ModelRequest {
    pub fn prepare(
        context: &ToolContext,
        operation: ModelOperation,
        arguments: &Value,
    ) -> Result<Self, ToolError> {
        let args = arguments
            .as_array()
            .ok_or_else(|| invalid("model arguments must be an array"))?;
        match operation {
            ModelOperation::List | ModelOperation::Available => Ok(Self::List {
                kind: model_type(args.first())?,
                provider: optional_provider(args.get(1))?,
                available: matches!(operation, ModelOperation::Available),
            }),
            ModelOperation::Find => Ok(Self::Find {
                kind: model_type(args.first())?,
                provider: required_string(args.get(1), "provider")?.into(),
                model_id: required_string(args.get(2), "model ID")?.into(),
            }),
            ModelOperation::Classify | ModelOperation::Images => {
                let model = args
                    .first()
                    .ok_or_else(|| invalid("a model with provider and id is required"))?;
                let provider =
                    ProviderId::new(required_string(model.get("provider"), "model.provider")?);
                let model_id = ModelId::new(required_string(model.get("id"), "model.id")?);
                let kind = if matches!(operation, ModelOperation::Classify) {
                    ModelType::Classifier
                } else {
                    ModelType::Image
                };
                // Validate against the frozen catalogue. Extra fields on the
                // supplied object are deliberately ignored, as in Pi.
                if context
                    .models
                    .model_of_type(kind, &provider, &model_id)?
                    .is_none()
                {
                    for other in [ModelType::Chat, ModelType::Classifier, ModelType::Image] {
                        if other != kind
                            && context
                                .models
                                .model_of_type(other, &provider, &model_id)?
                                .is_some()
                        {
                            return Err(invalid(format!(
                                "{provider}/{model_id} is a {} model; models.{} requires a {} model",
                                kind_name(other),
                                operation.name(),
                                kind_name(kind)
                            )));
                        }
                    }
                    return Err(invalid(format!(
                        "Unknown {} model {provider}/{model_id}; use models.getAvailableOfType({})",
                        kind_name(kind),
                        json!(kind_name(kind))
                    )));
                }
                let input = args
                    .get(1)
                    .filter(|value| value.is_object())
                    .ok_or_else(|| invalid("a context object is required"))?;
                // Validation precedes both call rows and provider admission.
                if matches!(operation, ModelOperation::Classify) {
                    // serde structs also accept sequences; the script contract
                    // requires bool criteria to be an object with named labels.
                    if input
                        .get("questions")
                        .and_then(Value::as_object)
                        .is_some_and(|questions| {
                            questions.values().any(|question| {
                                question["type"] == "bool" && !question["criteria"].is_object()
                            })
                        })
                    {
                        return Err(invalid(
                            "bool criteria must be {true: string, false: string}",
                        ));
                    }
                    let parsed: ClassifierContext = serde_json::from_value(input.clone()).map_err(|_| invalid("models.classify expects { state: object, images?: image[], questions: {id: {type, instructions, criteria}} }"))?;
                    parsed.validate().map_err(invalid)?;
                    Ok(Self::Classifier {
                        provider,
                        model_id,
                        input: parsed,
                    })
                } else {
                    let parsed: ImagesContext = serde_json::from_value(input.clone()).map_err(|_| invalid("models.generateImages expects { input: [{type: 'text', text}, ...image blocks] }"))?;
                    parsed.validate().map_err(invalid)?;
                    Ok(Self::Images {
                        provider,
                        model_id,
                        input: parsed,
                    })
                }
            }
        }
    }
}

impl ModelCalls {
    pub async fn execute(
        &self,
        context: &ToolContext,
        id: ToolCallId,
        request: ModelRequest,
        signal: AbortSignal,
        generated_images: &AtomicUsize,
    ) -> Result<NestedToolResult, ToolError> {
        let request = match request {
            ModelRequest::List {
                kind,
                provider,
                available,
            } => {
                let models = if available {
                    context
                        .models
                        .available_models_of_type(kind, provider.as_ref())?
                } else {
                    context.models.models_of_type(kind, provider.as_ref())?
                };
                return Ok(result(id, Some(to_value(models)?), None, false));
            }
            ModelRequest::Find {
                kind,
                provider,
                model_id,
            } => {
                let model = context
                    .models
                    .model_of_type(kind, &provider, &model_id)?
                    .map(to_value)
                    .transpose()?;
                return Ok(result(id, model, None, false));
            }
            inference => inference,
        };
        let _permit = tokio::select! {
            biased;
            () = signal.wait() => return Err(ToolError::Aborted),
            () = context.signal().wait() => return Err(ToolError::Aborted),
            permit = self.inference.acquire() => permit.map_err(|_| invalid("model calls are unavailable"))?,
        };
        let (value, usage) = match request {
            ModelRequest::Classifier {
                provider,
                model_id,
                input,
            } => {
                let result = context
                    .models
                    .classify(provider, model_id, input, signal)
                    .await?;
                let usage = result.usage.clone();
                (to_value(result)?, usage)
            }
            ModelRequest::Images {
                provider,
                model_id,
                input,
            } => {
                let result = context
                    .models
                    .generate_images(provider, model_id, input, signal)
                    .await?;
                generated_images.fetch_add(
                    result
                        .output
                        .iter()
                        .filter(|block| matches!(block, pi_core::ContentBlock::Image(_)))
                        .count(),
                    Ordering::Relaxed,
                );
                let usage = result.usage.clone();
                (to_value(result)?, usage)
            }
            ModelRequest::List { .. } | ModelRequest::Find { .. } => {
                unreachable!("catalog queries returned above")
            }
        };
        // The ordinary successful result carries usage itself. This ledger is
        // used by the parent scheduler only on interruption, avoiding double billing.
        if let Some(usage) = &usage {
            context.record_usage(usage);
        }
        let failed = value["stopReason"] != "stop";
        Ok(result(id, Some(value), usage, failed))
    }
}

fn result(
    id: ToolCallId,
    value: Option<Value>,
    usage: Option<Usage>,
    is_error: bool,
) -> NestedToolResult {
    NestedToolResult {
        call_id: id,
        result: ToolResult {
            usage,
            is_error,
            ..ToolResult::text("")
        },
        // The envelope preserves undefined for an unknown catalogue entry.
        structured_content: Some(json!({"found":value.is_some(), "value":value})),
    }
}

pub(crate) fn reference_summary(arguments: &Value) -> String {
    let model = &arguments[0];
    match (model["provider"].as_str(), model["id"].as_str()) {
        (Some(provider), Some(id)) => format!("{provider}/{id}"),
        _ => "unknown model".into(),
    }
}

fn model_type(value: Option<&Value>) -> Result<ModelType, ToolError> {
    match value.and_then(Value::as_str) {
        Some("chat") => Ok(ModelType::Chat),
        Some("classifier") => Ok(ModelType::Classifier),
        Some("image") => Ok(ModelType::Image),
        _ => Err(invalid("model type must be chat, classifier, or image")),
    }
}

fn kind_name(kind: ModelType) -> &'static str {
    match kind {
        ModelType::Chat => "chat",
        ModelType::Classifier => "classifier",
        ModelType::Image => "image",
    }
}

fn optional_provider(value: Option<&Value>) -> Result<Option<ProviderId>, ToolError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(required_string(Some(value), "provider")?.into())),
    }
}

fn required_string<'a>(value: Option<&'a Value>, field: &str) -> Result<&'a str, ToolError> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{field} must be a string")))
}

fn to_value(value: impl Serialize) -> Result<Value, ToolError> {
    serde_json::to_value(value).map_err(|_| invalid("model result cannot be represented as JSON"))
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments(message.into())
}
