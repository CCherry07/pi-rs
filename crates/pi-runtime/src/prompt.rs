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
    pub selected_tools: &'a [String],
    pub active_tools: &'a [ToolSpec],
    pub available_tools: &'a [ToolSpec],
    pub hidden_tools: &'a [String],
}

#[derive(Default)]
pub struct PromptOutput {
    pub system_prompt: String,
    /// Domain-defined, serialized inspection data, for example JS extension queries.
    pub options: Option<Value>,
    pub sections: Option<Vec<(String, String)>>,
    pub active_tools: Option<Vec<String>>,
    pub forced: bool,
    pub contributions: Vec<pi_core::PromptContribution>,
}

impl PromptOutput {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            options: None,
            ..Self::default()
        }
    }
}

/// Renders from prepared inputs without reloading resources or mutating the base.
pub trait SystemPromptRenderer: Send + Sync {
    fn render(&self, context: PromptContext<'_>) -> Result<PromptOutput, String>;

    fn render_options(
        &self,
        _context: PromptContext<'_>,
        _options: &Value,
    ) -> Result<PromptOutput, String> {
        Err("This system prompt does not expose mutable rendering options".into())
    }

    fn render_selected_options(
        &self,
        context: PromptContext<'_>,
        _options: &Value,
    ) -> Result<PromptOutput, String> {
        self.render(context)
    }
    fn force_options(&self, _options: &Value, _text: &str) -> Option<Value> {
        None
    }
}

impl<F> SystemPromptRenderer for F
where
    F: Fn(PromptContext<'_>) -> Result<PromptOutput, String> + Send + Sync,
{
    fn render(&self, context: PromptContext<'_>) -> Result<PromptOutput, String> {
        self(context)
    }
}

#[derive(Clone)]
pub struct PreparedSystemPrompt {
    /// Frozen source fragments, attributed only when present in the final request.
    pub prompt_contributions: Vec<pi_core::PromptContribution>,
    renderer: Arc<dyn SystemPromptRenderer>,
    pub resource_options: Option<Value>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

impl PreparedSystemPrompt {
    pub fn new(renderer: impl SystemPromptRenderer + 'static) -> Self {
        Self {
            renderer: Arc::new(renderer),
            prompt_contributions: Vec::new(),
            resource_options: None,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn render(&self, context: PromptContext<'_>) -> Result<PromptOutput, String> {
        self.renderer.render(context)
    }

    pub(crate) fn resolver(
        &self,
        workspace: WorkspaceSnapshot,
        registries: Arc<pi_plugin::FrozenRegistries>,
    ) -> Arc<dyn pi_plugin::SystemPromptResolver> {
        Arc::new(GenerationPromptResolver {
            prompt: self.clone(),
            workspace,
            registries,
        })
    }
}

struct GenerationPromptResolver {
    prompt: PreparedSystemPrompt,
    workspace: WorkspaceSnapshot,
    registries: Arc<pi_plugin::FrozenRegistries>,
}

impl GenerationPromptResolver {
    fn render_with(
        &self,
        active: &[String],
        options: Option<&Value>,
        replace_selection: bool,
    ) -> Result<pi_plugin::RenderedSystemPrompt, String> {
        let available = self.registries.all_tool_specs();
        let mut selection = active.to_vec();
        let mut options = options.cloned();
        let mut replace_selection = replace_selection;
        for _ in 0..2 {
            let loadout = self
                .registries
                .prepare_tool_loadout(&selection)
                .map_err(|error| error.to_string())?;
            let context = PromptContext {
                workspace: &self.workspace,
                selected_tools: &selection,
                active_tools: &loadout.tools,
                available_tools: &available,
                hidden_tools: &loadout.hidden_tools,
            };
            let output = match options.as_ref() {
                Some(options) if replace_selection => self
                    .prompt
                    .renderer
                    .render_selected_options(context, options)?,
                Some(options) => self.prompt.renderer.render_options(context, options)?,
                None => self.prompt.render(context)?,
            };
            let requested = output.active_tools.as_deref().unwrap_or(&selection);
            let mut seen = std::collections::HashSet::new();
            let selected = active
                .iter()
                .filter(|name| requested.contains(name))
                .chain(requested)
                .filter(|name| {
                    self.registries.tool_discovery(name).is_some() && seen.insert((*name).clone())
                })
                .cloned()
                .collect::<Vec<_>>();
            if requested != selected || selected != selection {
                selection = selected;
                options = output.options.or(options);
                replace_selection = true;
                continue;
            }
            let mut contributions = self.prompt.prompt_contributions.clone();
            contributions.extend(output.contributions);
            return Ok(pi_plugin::RenderedSystemPrompt {
                text: output.system_prompt,
                options: output.options.unwrap_or(Value::Null),
                sections: output.sections,
                forced: output.forced,
                active_tools: selection,
                hidden_tools: loadout.hidden_tools,
                contributions,
            });
        }
        Err("System prompt rendering did not stabilize its tool selection".into())
    }
}

impl pi_plugin::SystemPromptResolver for GenerationPromptResolver {
    fn render(
        &self,
        active_tools: &[String],
        options: Option<&Value>,
    ) -> Result<pi_plugin::RenderedSystemPrompt, String> {
        self.render_with(active_tools, options, false)
    }
    fn render_selection(
        &self,
        active_tools: &[String],
        options: &Value,
    ) -> Result<pi_plugin::RenderedSystemPrompt, String> {
        self.render_with(active_tools, Some(options), true)
    }
    fn force_options(&self, options: &Value, text: &str) -> Option<Value> {
        self.prompt.renderer.force_options(options, text)
    }
}
