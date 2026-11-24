//! Typed non-chat model values. Existing chat selections and session wire values stay unchanged.
use std::collections::BTreeMap;

use indexmap::IndexMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    ContentBlock, ImageContent, ModelCost, ModelId, ModelInput, ModelInputLimits, ModelSpec,
    ProviderId, Usage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Chat,
    Classifier,
    Image,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierModelSpec {
    pub provider: ProviderId,
    pub id: ModelId,
    pub name: String,
    pub api: String,
    pub base_url: Option<String>,
    pub input: Vec<ModelInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    pub context_window: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

impl ClassifierModelSpec {
    pub fn new(
        provider: impl Into<ProviderId>,
        id: impl Into<ModelId>,
        name: impl Into<String>,
        api: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            name: name.into(),
            api: api.into(),
            base_url: None,
            input: vec![ModelInput::Text],
            input_limits: None,
            cost: ModelCost::default(),
            context_window: 128_000,
            headers: BTreeMap::new(),
            compat: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageModelSpec {
    pub provider: ProviderId,
    pub id: ModelId,
    pub name: String,
    pub api: String,
    pub base_url: Option<String>,
    pub input: Vec<ModelInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub output: Vec<ModelInput>,
    pub cost: ModelCost,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl ImageModelSpec {
    pub fn new(
        provider: impl Into<ProviderId>,
        id: impl Into<ModelId>,
        name: impl Into<String>,
        api: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
            name: name.into(),
            api: api.into(),
            base_url: None,
            input: vec![ModelInput::Text],
            input_limits: None,
            output: vec![ModelInput::Image],
            cost: ModelCost::default(),
            headers: BTreeMap::new(),
        }
    }
}

/// A typed, credential-free catalog entry. Chat-only APIs continue to expose `ModelSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnyModelSpec {
    Chat(ModelSpec),
    Classifier(ClassifierModelSpec),
    Image(ImageModelSpec),
}

impl AnyModelSpec {
    pub fn model_type(&self) -> ModelType {
        match self {
            Self::Chat(_) => ModelType::Chat,
            Self::Classifier(_) => ModelType::Classifier,
            Self::Image(_) => ModelType::Image,
        }
    }
    pub fn provider(&self) -> &ProviderId {
        match self {
            Self::Chat(m) => &m.provider,
            Self::Classifier(m) => &m.provider,
            Self::Image(m) => &m.provider,
        }
    }
    pub fn id(&self) -> &ModelId {
        match self {
            Self::Chat(m) => &m.id,
            Self::Classifier(m) => &m.id,
            Self::Image(m) => &m.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierQuestion {
    Choice {
        instructions: String,
        criteria: IndexMap<String, String>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    Bool {
        instructions: String,
        criteria: ClassifierBoolCriteria,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierBoolCriteria {
    #[serde(rename = "true")]
    pub when_true: String,
    #[serde(rename = "false")]
    pub when_false: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierContext {
    pub state: Map<String, Value>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        with = "classifier_images"
    )]
    pub images: Vec<ImageContent>,
    pub questions: IndexMap<String, ClassifierQuestion>,
}

impl ClassifierContext {
    pub fn validate(&self) -> Result<(), String> {
        if self.questions.is_empty() {
            return Err("questions must be a non-empty object".into());
        }
        for (id, question) in &self.questions {
            let empty = match question {
                ClassifierQuestion::Choice { criteria, .. } => criteria.is_empty(),
                ClassifierQuestion::Score { criteria, .. } => criteria.is_empty(),
                ClassifierQuestion::Bool { .. } => false,
            };
            if empty {
                return Err(format!("question {id}: criteria must be non-empty"));
            }
        }
        Ok(())
    }
}

mod classifier_images {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        images: &[ImageContent],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        images
            .iter()
            .cloned()
            .map(ContentBlock::Image)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<ImageContent>, D::Error> {
        Vec::<ContentBlock>::deserialize(deserializer)?
            .into_iter()
            .map(|block| match block {
                ContentBlock::Image(image) => Ok(image),
                _ => Err(serde::de::Error::custom(
                    "classifier images must contain only image blocks",
                )),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierAnswer {
    Choice {
        choice: String,
        probabilities: IndexMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        confidence: f64,
    },
    Bool {
        probability: f64,
    },
}

/// Answers retain question insertion order, as in the model's JSON object.
pub type ClassifierAnswers = IndexMap<String, ClassifierAnswer>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelCallStopReason {
    Stop,
    Error,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierResult {
    pub api: String,
    pub provider: ProviderId,
    pub model: ModelId,
    pub answers: ClassifierAnswers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ModelCallStopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
}

impl ClassifierResult {
    pub fn failure(
        model: &ClassifierModelSpec,
        error: String,
        reason: ModelCallStopReason,
        timestamp_ms: i64,
    ) -> Self {
        Self {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            answers: IndexMap::new(),
            usage: None,
            stop_reason: reason,
            error_message: Some(error),
            timestamp_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImagesContext {
    pub input: Vec<ContentBlock>,
}

impl ImagesContext {
    pub fn validate(&self) -> Result<(), String> {
        if self.input.is_empty() {
            return Err("input must be a non-empty array".into());
        }
        if self
            .input
            .iter()
            .any(|block| !matches!(block, ContentBlock::Text(_) | ContentBlock::Image(_)))
        {
            return Err("input must contain only text or image blocks".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesResult {
    pub api: String,
    pub provider: ProviderId,
    pub model: ModelId,
    pub output: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ModelCallStopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(rename = "timestamp")]
    pub timestamp_ms: i64,
}

impl ImagesResult {
    pub fn failure(
        model: &ImageModelSpec,
        error: String,
        reason: ModelCallStopReason,
        timestamp_ms: i64,
    ) -> Self {
        Self {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: Vec::new(),
            response_id: None,
            usage: None,
            stop_reason: reason,
            error_message: Some(error),
            timestamp_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn classifier_context() -> Value {
        json!({
            "state": {"text": "Clear skies"},
            "images": [{"type": "image", "data": "cG5n", "mimeType": "image/png"}],
            "questions": {
                "weather": {"type": "choice", "instructions": "Choose weather", "criteria": {"sunny": "Clear skies"}},
                "quality": {"type": "score", "instructions": "Rate the image", "criteria": ["poor", "good"]},
                "outside": {"type": "bool", "instructions": "Is it outdoors?", "criteria": {"true": "Outside", "false": "Inside"}}
            }
        })
    }

    #[test]
    fn classifier_maps_keep_question_label_answer_and_probability_order() {
        let context: ClassifierContext = serde_json::from_str(r#"{"state":{},"questions":{"z":{"type":"choice","instructions":"","criteria":{"z":"last","a":"first"}},"a":{"type":"score","instructions":"","criteria":["low","high"]}}}"#).unwrap();
        assert_eq!(
            context
                .questions
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        let ClassifierQuestion::Choice { criteria, .. } = &context.questions["z"] else {
            panic!("choice")
        };
        assert_eq!(
            criteria.keys().map(String::as_str).collect::<Vec<_>>(),
            ["z", "a"]
        );
        let answers: ClassifierAnswers = serde_json::from_str(r#"{"z":{"type":"choice","choice":"z","confidence":0.9,"probabilities":{"z":0.9,"a":0.1}},"a":{"type":"bool","probability":0.8}}"#).unwrap();
        assert_eq!(
            answers.keys().map(String::as_str).collect::<Vec<_>>(),
            ["z", "a"]
        );
        let ClassifierAnswer::Choice { probabilities, .. } = &answers["z"] else {
            panic!("choice")
        };
        assert_eq!(
            probabilities.keys().map(String::as_str).collect::<Vec<_>>(),
            ["z", "a"]
        );
        let serialized = serde_json::to_value(answers).unwrap();
        assert_eq!(
            serialized
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
    }

    #[test]
    fn classifier_context_round_trips_pi_question_and_image_blocks() {
        let value = classifier_context();
        let context: ClassifierContext = serde_json::from_value(value.clone()).unwrap();
        context.validate().unwrap();
        assert_eq!(context.images[0].mime_type, "image/png");
        assert_eq!(serde_json::to_value(context).unwrap(), value);
    }

    #[test]
    fn classifier_context_rejects_malformed_questions_and_images() {
        for (field, replacement) in [
            ("state", json!([])),
            (
                "questions",
                json!({"q": {"type": "unknown", "instructions": "", "criteria": {}}}),
            ),
            ("images", json!([{"type": "text", "text": "not an image"}])),
        ] {
            let mut value = classifier_context();
            value[field] = replacement;
            assert!(serde_json::from_value::<ClassifierContext>(value).is_err());
        }
        for questions in [
            json!({}),
            json!({"q": {"type": "choice", "instructions": "", "criteria": {}}}),
            json!({"q": {"type": "score", "instructions": "", "criteria": []}}),
        ] {
            let context: ClassifierContext =
                serde_json::from_value(json!({"state": {}, "questions": questions})).unwrap();
            assert!(context.validate().is_err());
        }
    }

    #[test]
    fn image_context_accepts_only_nonempty_text_and_image_input() {
        let context: ImagesContext = serde_json::from_value(json!({"input": [
            {"type": "text", "text": "Draw a tree"},
            {"type": "image", "data": "cG5n", "mimeType": "image/png"}
        ]}))
        .unwrap();
        context.validate().unwrap();
        assert!(ImagesContext { input: vec![] }.validate().is_err());
        let context: ImagesContext = serde_json::from_value(json!({"input": [
            {"type": "thinking", "thinking": "internal"}
        ]}))
        .unwrap();
        assert!(context.validate().is_err());
    }

    #[test]
    fn specialized_model_metadata_round_trips_without_erasing_typed_base_fields() {
        let classifier_value = json!({
            "provider":"custom", "id":"judge", "name":"Judge", "api":"typesafe-system-one",
            "baseUrl":"https://classifier.test/v1", "input":["text","image"],
            "inputLimits":{"maxRequestBytes":4096,"images":{"maxPerRequest":2}},
            "cost":{"input":1.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},
            "contextWindow":64000, "headers":{"X-Model":"judge"},
            "compat":{"imageInput":"images"}
        });
        let classifier: ClassifierModelSpec =
            serde_json::from_value(classifier_value.clone()).unwrap();
        assert_eq!(classifier.context_window, 64_000);
        assert_eq!(classifier.headers["X-Model"], "judge");
        assert_eq!(
            classifier
                .input_limits
                .unwrap()
                .images
                .unwrap()
                .max_per_request,
            Some(2)
        );
        assert_eq!(serde_json::to_value(classifier).unwrap(), classifier_value);

        let image_value = json!({
            "provider":"custom", "id":"artist", "name":"Artist", "api":"openrouter-images",
            "baseUrl":"https://images.test/v1", "input":["text","image"],
            "inputLimits":{"images":{"resize":{"maxWidth":2048,"maxHeight":2048,"maxBytes":4000000,"jpegQuality":90}}},
            "output":["image","text"],
            "cost":{"input":1.0,"output":2.0,"cacheRead":0.0,"cacheWrite":0.0},
            "headers":{"X-Model":"artist"}
        });
        let image: ImageModelSpec = serde_json::from_value(image_value.clone()).unwrap();
        assert_eq!(image.headers["X-Model"], "artist");
        assert_eq!(
            image
                .input_limits
                .unwrap()
                .images
                .unwrap()
                .resize
                .unwrap()
                .max_width,
            Some(2048)
        );
        assert_eq!(serde_json::to_value(image).unwrap(), image_value);
    }

    #[test]
    fn typed_catalog_keeps_chat_wire_and_discriminates_specialized_models() {
        let chat = ModelSpec::new("provider", "same", "Chat", "chat");
        assert!(serde_json::to_value(&chat).unwrap().get("type").is_none());
        for model in [
            AnyModelSpec::Chat(chat),
            AnyModelSpec::Classifier(ClassifierModelSpec::new(
                "provider",
                "same",
                "Judge",
                "classifier",
            )),
            AnyModelSpec::Image(ImageModelSpec::new("provider", "same", "Artist", "images")),
        ] {
            let value = serde_json::to_value(&model).unwrap();
            assert_eq!(
                value["type"],
                serde_json::to_value(model.model_type()).unwrap()
            );
            assert_eq!(
                serde_json::from_value::<AnyModelSpec>(value).unwrap(),
                model
            );
        }
    }

    #[test]
    fn specialized_failures_use_pi_result_fields() {
        let result = ClassifierResult::failure(
            &ClassifierModelSpec::new("provider", "judge", "Judge", "classifier"),
            "cancelled".into(),
            ModelCallStopReason::Aborted,
            123,
        );
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({
                "api": "classifier", "provider": "provider", "model": "judge", "answers": {},
                "stopReason": "aborted", "errorMessage": "cancelled", "timestamp": 123
            })
        );
        let result = ImagesResult::failure(
            &ImageModelSpec::new("provider", "artist", "Artist", "images"),
            "unavailable".into(),
            ModelCallStopReason::Error,
            456,
        );
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({
                "api": "images", "provider": "provider", "model": "artist", "output": [],
                "stopReason": "error", "errorMessage": "unavailable", "timestamp": 456
            })
        );
    }
}
