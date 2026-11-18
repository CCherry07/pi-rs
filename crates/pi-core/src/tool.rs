use crate::{ContentBlock, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSchemaStrict {
    Prefer,
    Require,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GrammarFormat {
    #[serde(rename = "openai_lark")]
    OpenAiLark,
    #[serde(rename = "openai_regex")]
    OpenAiRegex,
}

/// Requested provider-side argument sampling, independent of execution validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstrainedSampling {
    Disabled,
    JsonSchema {
        strict: ToolSchemaStrict,
    },
    Grammar {
        variants: std::collections::BTreeMap<GrammarFormat, String>,
    },
}

impl Serialize for ConstrainedSampling {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Disabled => false.serialize(serializer),
            Self::JsonSchema { strict } => {
                serde_json::json!({"type":"json_schema","strict":strict}).serialize(serializer)
            }
            Self::Grammar { variants } => {
                serde_json::json!({"type":"grammar","variants":variants}).serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ConstrainedSampling {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Config {
            JsonSchema {
                strict: ToolSchemaStrict,
            },
            Grammar {
                variants: std::collections::BTreeMap<GrammarFormat, String>,
            },
        }
        let value = Value::deserialize(deserializer)?;
        if value == Value::Bool(false) {
            return Ok(Self::Disabled);
        }
        match serde_json::from_value(value).map_err(serde::de::Error::custom)? {
            Config::JsonSchema { strict } => Ok(Self::JsonSchema { strict }),
            Config::Grammar { variants } => Ok(Self::Grammar { variants }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    pub output_schema: Option<Value>,
    pub constrained_sampling: Option<ConstrainedSampling>,
    pub execution_mode: ToolExecutionMode,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    pub structured_content: Option<Value>,
    pub details: Option<Value>,
    pub usage: Option<Usage>,
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: bool,
    pub terminate: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(crate::TextContent::new(text))],
            structured_content: None,
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: false,
            terminate: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(text)
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUpdate {
    pub content: Vec<ContentBlock>,
    pub details: Option<Value>,
}
