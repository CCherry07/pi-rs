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
        docs_path: &std::path::Path,
    ) -> Result<Self, ToolError> {
        let args = arguments
            .as_array()
            .ok_or_else(|| invalid("model arguments must be an array"))?;
        match operation {
            ModelOperation::List | ModelOperation::Available => Ok(Self::List {
                kind: model_type(args.first())?,
                provider: optional_provider(args.get(1))?.filter(|provider| {
                    !matches!(operation, ModelOperation::Available) || !provider.as_str().is_empty()
                }),
                available: matches!(operation, ModelOperation::Available),
            }),
            ModelOperation::Find => {
                let (Some(provider), Some(id)) = (
                    args.get(1).and_then(Value::as_str),
                    args.get(2).and_then(Value::as_str),
                ) else {
                    return Err(invalid(format!(
                        "models.getModelOfType(type, provider, id) expects three strings, got ({}). The provider and the id are separate arguments, for example models.getModelOfType(\"classifier\", \"typesafe\", \"jev-latest\").",
                        args.iter()
                            .map(|value| describe_value(Some(value)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                };
                Ok(Self::Find {
                    kind: model_type(args.first())?,
                    provider: provider.into(),
                    model_id: id.into(),
                })
            }
            ModelOperation::Classify | ModelOperation::Images => {
                let model = args.first();
                let kind = if matches!(operation, ModelOperation::Classify) {
                    ModelType::Classifier
                } else {
                    ModelType::Image
                };
                let hint = format!(
                    "List the {} models you can use with models.getAvailableOfType(\"{}\").",
                    kind_name(kind),
                    kind_name(kind)
                );
                let (Some(provider), Some(id)) = (
                    model
                        .and_then(|model| model.get("provider"))
                        .and_then(Value::as_str),
                    model
                        .and_then(|model| model.get("id"))
                        .and_then(Value::as_str),
                ) else {
                    let undefined_hint = if model.is_none_or(Value::is_null) {
                        " models.getModelOfType() returns undefined for an unknown provider or id."
                    } else {
                        ""
                    };
                    return Err(invalid(format!(
                        "models.{}() expects {} model as its first argument, got {}.{undefined_hint} {hint}",
                        operation.name(),
                        with_article(kind),
                        describe_value(model)
                    )));
                };
                let provider = ProviderId::new(provider);
                let model_id = ModelId::new(id);
                // Validate against the frozen catalogue. Extra fields on the
                // supplied object are deliberately ignored, as in Pi.
                let resolved = context.models.model_of_type(kind, &provider, &model_id)?;
                let Some(resolved) = resolved else {
                    for other in [ModelType::Chat, ModelType::Image, ModelType::Classifier] {
                        if other != kind
                            && context
                                .models
                                .model_of_type(other, &provider, &model_id)?
                                .is_some()
                        {
                            return Err(invalid(format!(
                                "\"{provider}/{model_id}\" is {} model, not {} model. {hint}",
                                with_article(other),
                                with_article(kind)
                            )));
                        }
                    }
                    return Err(invalid(format!(
                        "Unknown {} model \"{provider}/{model_id}\". {hint}",
                        kind_name(kind)
                    )));
                };
                // Use the canonical host identity, never transport fields supplied by the script.
                let provider = resolved.provider().clone();
                let model_id = resolved.id().clone();
                let input = args.get(1);
                // Validation precedes both call rows and provider admission.
                if matches!(operation, ModelOperation::Classify) {
                    let parsed = classifier_context(input, docs_path)?;
                    Ok(Self::Classifier {
                        provider,
                        model_id,
                        input: parsed,
                    })
                } else {
                    let parsed = images_context(input, docs_path)?;
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
                        .query_available_models_of_type(kind, provider.as_ref(), signal)
                        .await?
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
        _ => Err(invalid(format!(
            "Unknown model type {}. Use \"chat\", \"image\", or \"classifier\".",
            stringify_optional(value)
        ))),
    }
}

fn kind_name(kind: ModelType) -> &'static str {
    match kind {
        ModelType::Chat => "chat",
        ModelType::Classifier => "classifier",
        ModelType::Image => "image",
    }
}

fn with_article(kind: ModelType) -> &'static str {
    match kind {
        ModelType::Chat => "a chat",
        ModelType::Classifier => "a classifier",
        ModelType::Image => "an image",
    }
}

fn stringify_optional(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".into(), crate::declarations::json_stringify)
}

fn describe_value(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(_)) => "a string".into(),
        Some(Value::Bool(_)) => "a boolean".into(),
        Some(Value::Number(_)) => "a number".into(),
        Some(Value::Array(values)) => if values.is_empty() {
            "an empty array"
        } else {
            "an array"
        }
        .into(),
        Some(Value::Object(fields)) if fields.is_empty() => "{}".into(),
        Some(Value::Object(fields)) => format!(
            "{{ {}{} }}",
            fields
                .keys()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if fields.len() > 6 { ", ..." } else { "" }
        ),
    }
}

const CLASSIFIER_CONTEXT_SHAPE: &str = "{ state: { ... }, images?: [{ type: \"image\", data: <base64>, mimeType }], questions: { <id>: { type: \"choice\", instructions, criteria: { <label>: <meaning> } } | { type: \"score\", instructions, criteria: [<lowest level>, ..., <highest level>] } | { type: \"bool\", instructions, criteria: { true: <meaning>, false: <meaning> } } } }";

fn classifier_context(
    input: Option<&Value>,
    docs_path: &std::path::Path,
) -> Result<ClassifierContext, ToolError> {
    let fail = |problem: String| {
        invalid(format!(
            "models.classify() {problem}. Expected context: {CLASSIFIER_CONTEXT_SHAPE}. See \"Classify\" in {}.",
            docs_path.to_string_lossy().replace('\\', "/")
        ))
    };
    let context = input.and_then(Value::as_object).ok_or_else(|| {
        fail(format!(
            "expects a context object as its second argument, got {}",
            describe_value(input)
        ))
    })?;
    let state = context
        .get("state")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            fail(format!(
                "context.state must be an object, got {}",
                describe_value(context.get("state"))
            ))
        })?;
    let mut images = Vec::new();
    if let Some(value) = context.get("images") {
        let values = value.as_array().ok_or_else(|| {
            fail(format!(
                "context.images must be an array, got {}",
                describe_value(Some(value))
            ))
        })?;
        for (index, image) in values.iter().enumerate() {
            if !is_image(image) {
                return Err(fail(format!(
                    "context.images[{index}] must be an image block, got {}",
                    describe_value(Some(image))
                )));
            }
            images.push(image_block(image));
        }
    }
    let questions = context
        .get("questions")
        .and_then(Value::as_object)
        .filter(|questions| !questions.is_empty())
        .ok_or_else(|| {
            fail(format!(
                "context.questions must map question IDs to questions, got {}",
                describe_value(context.get("questions"))
            ))
        })?;
    for (id, question) in questions {
        let at = format!("context.questions.{id}");
        let question = question.as_object().ok_or_else(|| {
            fail(format!(
                "{at} must be a question object, got {}",
                describe_value(Some(question))
            ))
        })?;
        if !question.get("instructions").is_some_and(Value::is_string) {
            return Err(fail(format!("{at}.instructions must be a string")));
        }
        let criteria = question.get("criteria");
        match question.get("type").and_then(Value::as_str) {
            Some("choice") => {
                if !criteria.and_then(Value::as_object).is_some_and(|values| {
                    !values.is_empty() && values.values().all(Value::is_string)
                }) {
                    return Err(fail(format!(
                        "{at} is a \"choice\" question, so criteria must map each label to its meaning"
                    )));
                }
            }
            Some("score") => {
                if !criteria
                    .and_then(Value::as_array)
                    .is_some_and(|values| !values.is_empty() && values.iter().all(Value::is_string))
                {
                    return Err(fail(format!(
                        "{at} is a \"score\" question, so criteria must list the levels as strings, lowest first"
                    )));
                }
            }
            Some("bool") => {
                if !criteria.is_some_and(|value| {
                    value.get("true").is_some_and(Value::is_string)
                        && value.get("false").is_some_and(Value::is_string)
                }) {
                    return Err(fail(format!(
                        "{at} is a \"bool\" question, so criteria must be {{ true: string, false: string }}"
                    )));
                }
            }
            _ => {
                return Err(fail(format!(
                    "{at}.type must be \"choice\", \"score\", or \"bool\", got {}",
                    stringify_optional(question.get("type"))
                )));
            }
        }
    }
    // Project only the documented model inputs. Session ContentBlock metadata has
    // stronger types and must not reject irrelevant properties on a script block.
    serde_json::from_value(json!({"state":state,"images":images,"questions":questions}))
        .map_err(|error| invalid(error.to_string()))
}

fn is_image(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("image")
        && value.get("data").is_some_and(Value::is_string)
        && value.get("mimeType").is_some_and(Value::is_string)
}

fn image_block(value: &Value) -> Value {
    json!({"type":"image", "data":value["data"], "mimeType":value["mimeType"]})
}

fn images_context(
    input: Option<&Value>,
    docs_path: &std::path::Path,
) -> Result<ImagesContext, ToolError> {
    let fail = |problem: String| {
        invalid(format!(
            "models.generateImages() {problem}. Expected context: {{ input: [{{ type: \"text\", text: <prompt> }}, ...optional {{ type: \"image\", data: <base64>, mimeType }} references] }}. See \"Generate images\" in {}.",
            docs_path.to_string_lossy().replace('\\', "/")
        ))
    };
    let context = input.and_then(Value::as_object).ok_or_else(|| {
        fail(format!(
            "expects a context object as its second argument, got {}",
            describe_value(input)
        ))
    })?;
    let values = context
        .get("input")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(|| {
            fail(format!(
                "context.input must be a non-empty array of blocks, got {}",
                describe_value(context.get("input"))
            ))
        })?;
    let mut blocks = Vec::new();
    for (index, block) in values.iter().enumerate() {
        if block.get("type").and_then(Value::as_str) == Some("text")
            && block.get("text").is_some_and(Value::is_string)
        {
            let mut projected = json!({"type":"text", "text":block["text"]});
            if let Some(signature) = block.get("textSignature") {
                if !signature.is_string() {
                    return Err(fail(format!(
                        "context.input[{index}].textSignature must be a string"
                    )));
                }
                projected["textSignature"] = signature.clone();
            }
            blocks.push(projected);
        } else if is_image(block) {
            blocks.push(image_block(block));
        } else {
            return Err(fail(format!(
                "context.input[{index}] must be a text or image block, got {}",
                describe_value(Some(block))
            )));
        }
    }
    serde_json::from_value(json!({"input":blocks})).map_err(|error| invalid(error.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn message(error: ToolError) -> String {
        let ToolError::InvalidArguments(message) = error else {
            panic!("expected argument error")
        };
        message
    }

    #[test]
    fn empty_provider_means_all_only_for_available_queries() {
        let (_, signal) = pi_core::AbortHandle::new();
        let context = ToolContext::standalone(std::env::temp_dir(), signal);
        for (operation, expected) in [
            (ModelOperation::List, Some("")),
            (ModelOperation::Available, None),
        ] {
            let request = ModelRequest::prepare(
                &context,
                operation,
                &json!(["classifier", ""]),
                Path::new("codemode.md"),
            )
            .unwrap();
            let ModelRequest::List { provider, .. } = request else {
                panic!("catalog query")
            };
            assert_eq!(provider.as_ref().map(ProviderId::as_str), expected);
        }
    }

    #[test]
    fn validation_reports_specific_shape_and_documentation() {
        let docs = Path::new("docs/codemode.md");
        let error = message(classifier_context(Some(&json!({"state":{},"questions":{"q":{"type":"choice","instructions":"","criteria":[]}}})), docs).unwrap_err());
        assert!(error.starts_with("models.classify() context.questions.q is a \"choice\" question, so criteria must map each label to its meaning. Expected context:"));
        assert!(error.ends_with("See \"Classify\" in docs/codemode.md."));
        let error = message(
            images_context(Some(&json!({"input":[{"type":"text","text":3}]})), docs).unwrap_err(),
        );
        assert!(error.starts_with("models.generateImages() context.input[0] must be a text or image block, got { type, text }."));
        assert_eq!(
            message(model_type(None).unwrap_err()),
            "Unknown model type undefined. Use \"chat\", \"image\", or \"classifier\"."
        );
    }

    #[test]
    fn documented_inputs_preserve_valid_metadata_without_weakening_session_blocks() {
        let docs = Path::new("codemode.md");
        let input = images_context(
            Some(&json!({"input":[{"type":"text","text":"draw","textSignature":"signed"}]})),
            docs,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            json!({"input":[{"type":"text","text":"draw","textSignature":"signed"}]})
        );
        let error = message(
            images_context(
                Some(&json!({"input":[{"type":"text","text":"draw","textSignature":42}]})),
                docs,
            )
            .unwrap_err(),
        );
        assert!(error.starts_with(
            "models.generateImages() context.input[0].textSignature must be a string."
        ));
        let context = classifier_context(
            Some(&json!({"state":{},"questions":{
                "z":{"type":"choice","instructions":"","criteria":{"z":"last","a":"first"}},
                "a":{"type":"bool","instructions":"","criteria":{"true":"yes","false":"no"}}
            }})),
            Path::new("codemode.md"),
        )
        .unwrap();
        assert_eq!(
            context
                .questions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
    }
}
