//! Typed non-chat model values. Existing chat selections and session wire values stay unchanged.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    ContentBlock, ImageContent, ModelCost, ModelId, ModelInput, ModelSpec, ProviderId, Usage,
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
    pub cost: ModelCost,
    pub context_window: u64,
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
            cost: ModelCost::default(),
            context_window: 128_000,
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
    pub output: Vec<ModelInput>,
    pub cost: ModelCost,
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
            output: vec![ModelInput::Image],
            cost: ModelCost::default(),
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
        criteria: BTreeMap<String, String>,
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
    pub questions: BTreeMap<String, ClassifierQuestion>,
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
        probabilities: BTreeMap<String, f64>,
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
    pub answers: BTreeMap<String, ClassifierAnswer>,
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
            answers: BTreeMap::new(),
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
