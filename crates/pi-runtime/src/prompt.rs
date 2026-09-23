//! Domain-owned prompt preparation inside the existing runtime generation.

use std::{fmt, path::PathBuf, sync::Arc};

use pi_core::{ToolSpec, WorkspaceSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    Warning,
    Collision,
}

/// Presentation-neutral diagnostics supplied by the domain's resource preparation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    pub kind: DiagnosticKind,
    pub message: String,
    pub path: PathBuf,
}

#[derive(Clone)]
pub enum SystemPrompt {
    /// Use this exact final prompt without domain-specific assembly.
    Final(String),
    /// Prepare domain resources once per generation and render its selected tools.
    Dynamic(Arc<dyn SystemPromptFactory>),
}

impl SystemPrompt {
    pub fn dynamic(factory: impl SystemPromptFactory + 'static) -> Self {
        Self::Dynamic(Arc::new(factory))
    }
}

impl fmt::Debug for SystemPrompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Final(prompt) => f.debug_tuple("Final").field(prompt).finish(),
            Self::Dynamic(_) => f.write_str("Dynamic(..)"),
        }
    }
}

/// Prepares immutable prompt inputs for a candidate generation. This runs on the
/// same construction path as plugin factories and must not mutate a live runtime.
pub trait SystemPromptFactory: Send + Sync {
    fn prepare(&self, workspace: &WorkspaceSnapshot) -> Result<PreparedSystemPrompt, String>;
}

impl<F> SystemPromptFactory for F
where
    F: Fn(&WorkspaceSnapshot) -> Result<PreparedSystemPrompt, String> + Send + Sync,
{
    fn prepare(&self, workspace: &WorkspaceSnapshot) -> Result<PreparedSystemPrompt, String> {
        self(workspace)
    }
}

/// Only selected, registered tools are exposed, in selection order.
pub struct PromptContext<'a> {
    pub workspace: &'a WorkspaceSnapshot,
    pub active_tools: &'a [ToolSpec],
}

pub struct PromptOutput {
    pub system_prompt: String,
    /// Domain-defined, serialized inspection data, for example JS extension queries.
    pub options: Option<Value>,
}

impl PromptOutput {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            options: None,
        }
    }
}

/// Renders from prepared inputs without reloading resources or mutating the base.
pub trait SystemPromptRenderer: Send + Sync {
    fn render(&self, context: PromptContext<'_>) -> Result<PromptOutput, String>;
}

impl<F> SystemPromptRenderer for F
where
    F: Fn(PromptContext<'_>) -> Result<PromptOutput, String> + Send + Sync,
{
    fn render(&self, context: PromptContext<'_>) -> Result<PromptOutput, String> {
        self(context)
    }
}

pub struct PreparedSystemPrompt {
    renderer: Arc<dyn SystemPromptRenderer>,
    pub resource_options: Option<Value>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

impl PreparedSystemPrompt {
    pub fn new(renderer: impl SystemPromptRenderer + 'static) -> Self {
        Self {
            renderer: Arc::new(renderer),
            resource_options: None,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn render(&self, context: PromptContext<'_>) -> Result<PromptOutput, String> {
        self.renderer.render(context)
    }
}
