use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pi_plugin::Plugin;
use serde::{Deserialize, Serialize};

use crate::EvalGrader;

pub const EVAL_RUN_SCHEMA_VERSION: u32 = 1;

pub type EvalAgentPluginFactory = Arc<dyn Fn() -> Arc<dyn Plugin> + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EvalSystemPrompt {
    #[default]
    Default,
    WithoutPiDocumentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalVariant {
    pub name: String,
    pub system_prompt: EvalSystemPrompt,
    pub extensions: Vec<String>,
    pub native_plugins: Vec<PathBuf>,
}

impl EvalVariant {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            system_prompt: EvalSystemPrompt::Default,
            extensions: Vec::new(),
            native_plugins: Vec::new(),
        }
    }

    pub fn system_prompt(mut self, treatment: EvalSystemPrompt) -> Self {
        self.system_prompt = treatment;
        self
    }

    pub fn extension(mut self, source: impl Into<String>) -> Self {
        self.extensions.push(source.into());
        self
    }

    pub fn native_plugin(mut self, path: impl Into<PathBuf>) -> Self {
        self.native_plugins.push(path.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<(), EvalError> {
        if self.name.trim().is_empty() {
            return Err(EvalError::InvalidCase(
                "variant name must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

impl From<String> for EvalVariant {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

impl From<&str> for EvalVariant {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalFixture {
    Empty,
    Directory(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalStep {
    Prompt(String),
    /// Prompt with `{{workspace}}`, `{{agent_dir}}`, and `{{home}}` replaced
    /// by the isolated run paths.
    PromptTemplate(String),
    Reload,
    InvokeCommand {
        name: String,
        arguments: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalLimits {
    pub step_timeout: Duration,
}

impl Default for EvalLimits {
    fn default() -> Self {
        Self {
            step_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone)]
pub struct EvalCase {
    pub id: String,
    pub description: String,
    pub fixture: EvalFixture,
    pub steps: Vec<EvalStep>,
    pub graders: Vec<Arc<dyn EvalGrader>>,
    pub limits: EvalLimits,
    pub active_tools: Option<Vec<String>>,
    pub discover_extensions: bool,
    pub requires_js_host: bool,
    pub agent_plugins: Vec<EvalAgentPluginFactory>,
}

impl EvalCase {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            fixture: EvalFixture::Empty,
            steps: Vec::new(),
            graders: Vec::new(),
            limits: EvalLimits::default(),
            active_tools: None,
            discover_extensions: false,
            requires_js_host: false,
            agent_plugins: Vec::new(),
        }
    }

    pub fn fixture(mut self, fixture: EvalFixture) -> Self {
        self.fixture = fixture;
        self
    }

    pub fn step(mut self, step: EvalStep) -> Self {
        self.steps.push(step);
        self
    }

    pub fn grader(mut self, grader: impl EvalGrader + 'static) -> Self {
        self.graders.push(Arc::new(grader));
        self
    }

    pub fn active_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.active_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    pub fn discover_extensions(mut self, discover: bool) -> Self {
        self.discover_extensions = discover;
        self
    }

    pub fn requires_js_host(mut self, required: bool) -> Self {
        self.requires_js_host = required;
        self
    }

    pub fn plugin<F, P>(mut self, factory: F) -> Self
    where
        F: Fn() -> P + Send + Sync + 'static,
        P: Plugin + 'static,
    {
        self.agent_plugins
            .push(Arc::new(move || Arc::new(factory())));
        self
    }

    pub fn plugin_arc<F>(mut self, factory: F) -> Self
    where
        F: Fn() -> Arc<dyn Plugin> + Send + Sync + 'static,
    {
        self.agent_plugins.push(Arc::new(factory));
        self
    }

    pub fn limits(mut self, limits: EvalLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), EvalError> {
        if self.id.trim().is_empty() {
            return Err(EvalError::InvalidCase("case id must not be empty".into()));
        }
        if self
            .id
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || ".-_/".contains(character)))
        {
            return Err(EvalError::InvalidCase(format!(
                "case id contains unsupported characters: {}",
                self.id
            )));
        }
        if self.steps.is_empty() {
            return Err(EvalError::InvalidCase(format!(
                "case {} has no steps",
                self.id
            )));
        }
        if !self.steps.iter().any(|step| {
            matches!(
                step,
                EvalStep::Prompt(_) | EvalStep::PromptTemplate(_) | EvalStep::InvokeCommand { .. }
            )
        }) {
            return Err(EvalError::InvalidCase(format!(
                "case {} never submits input",
                self.id
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalExecutionOutcome {
    Completed,
    Errored,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalTranscriptEvent {
    Message {
        role: String,
        content: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        is_error: bool,
    },
    Custom {
        custom_type: String,
        content: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: Option<f64>,
    pub tool_calls: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceChangeKind {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChange {
    pub path: String,
    pub kind: WorkspaceChangeKind,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub before_text: Option<String>,
    pub after_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub final_response: String,
    pub transcript: Vec<EvalTranscriptEvent>,
    pub workspace_changes: Vec<WorkspaceChange>,
    pub usage: EvalUsage,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalGrade {
    pub grader: String,
    pub score: f64,
    pub passed: bool,
    pub required: bool,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactReference {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRun {
    pub schema_version: u32,
    pub run_id: String,
    pub case_id: String,
    pub variant: String,
    pub provider: String,
    pub model: String,
    pub repetition: u32,
    pub started_at_ms: i64,
    pub duration_ms: u64,
    pub execution_outcome: EvalExecutionOutcome,
    pub passed: bool,
    pub observation: EvalObservation,
    pub grades: Vec<EvalGrade>,
    pub artifacts: Vec<ArtifactReference>,
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("invalid eval case: {0}")]
    InvalidCase(String),
    #[error("eval fixture error: {0}")]
    Fixture(String),
    #[error("eval runtime error: {0}")]
    Runtime(String),
    #[error("eval artifact error: {0}")]
    Artifact(String),
}
