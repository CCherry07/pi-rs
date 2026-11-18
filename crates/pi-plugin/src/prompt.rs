//! Domain-neutral, run-local system prompt rendering.
use pi_core::{Message, PromptContribution, SystemMessage};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct RenderedSystemPrompt {
    pub text: String,
    pub options: Value,
    /// Complete ordered sections, including their presentation delimiters.
    pub sections: Option<Vec<(String, String)>>,
    /// A forced leading prompt is projected onto requests, never persisted.
    pub forced: bool,
    pub active_tools: Vec<String>,
    pub hidden_tools: Vec<String>,
    pub contributions: Vec<PromptContribution>,
}

/// A prepared generation supplies its own option schema and rendering policy.
pub trait SystemPromptResolver: Send + Sync {
    fn render(
        &self,
        active_tools: &[String],
        options: Option<&Value>,
    ) -> Result<RenderedSystemPrompt, String>;
    fn render_selection(
        &self,
        active_tools: &[String],
        options: &Value,
    ) -> Result<RenderedSystemPrompt, String> {
        self.render(active_tools, Some(options))
    }
    fn force_options(&self, _options: &Value, _text: &str) -> Option<Value> {
        None
    }
}

#[derive(Clone)]
pub struct RunSystemPrompt {
    resolver: Arc<dyn SystemPromptResolver>,
    rendered: RenderedSystemPrompt,
    text_override: Option<String>,
}

impl std::fmt::Debug for RunSystemPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunSystemPrompt")
            .field("rendered", &self.rendered)
            .field("text_override", &self.text_override)
            .finish_non_exhaustive()
    }
}
impl PartialEq for RunSystemPrompt {
    fn eq(&self, other: &Self) -> bool {
        self.rendered == other.rendered && self.text_override == other.text_override
    }
}

impl RunSystemPrompt {
    pub fn new(
        resolver: Arc<dyn SystemPromptResolver>,
        active_tools: &[String],
    ) -> Result<Self, String> {
        let rendered = resolver.render(active_tools, None)?;
        Ok(Self {
            resolver,
            rendered,
            text_override: None,
        })
    }
    pub fn text(&self) -> &str {
        self.text_override.as_deref().unwrap_or(&self.rendered.text)
    }
    pub fn options(&self) -> &Value {
        &self.rendered.options
    }
    pub fn active_tools(&self) -> &[String] {
        &self.rendered.active_tools
    }
    pub fn hidden_tools(&self) -> &[String] {
        &self.rendered.hidden_tools
    }
    pub fn contributions(&self) -> &[PromptContribution] {
        &self.rendered.contributions
    }
    pub fn is_forced(&self) -> bool {
        self.text_override.is_some() || self.rendered.forced
    }
    pub fn has_sections(&self) -> bool {
        self.rendered.sections.is_some()
    }
    pub fn with_options(&self, options: Value) -> Result<Self, String> {
        let rendered = self.resolver.render(self.active_tools(), Some(&options))?;
        Ok(Self {
            resolver: Arc::clone(&self.resolver),
            rendered,
            text_override: self.text_override.clone(),
        })
    }
    pub fn with_active_tools(&self, active_tools: &[String]) -> Result<Self, String> {
        let rendered = self
            .resolver
            .render_selection(active_tools, self.options())?;
        Ok(Self {
            resolver: Arc::clone(&self.resolver),
            rendered,
            text_override: self.text_override.clone(),
        })
    }
    pub fn with_text(mut self, text: String) -> Self {
        if let Some(options) = self.resolver.force_options(self.options(), &text)
            && let Ok(rendered) = self.resolver.render(self.active_tools(), Some(&options))
        {
            self.rendered = rendered;
            self.text_override = None;
        } else {
            self.text_override = Some(text);
        }
        self
    }

    /// Resume, branch and live runs all diff against replayed transcript state.
    pub fn section_update(&self, messages: &[Message], timestamp_ms: i64) -> Option<SystemMessage> {
        let desired = self.rendered.sections.as_ref()?;
        let mut previous: Vec<(String, String)> = Vec::new();
        for message in messages {
            if let Message::System(message) = message {
                for (name, value) in &message.sections {
                    if let Some(position) = previous.iter().position(|(key, _)| key == name) {
                        if let Some(value) = value {
                            previous[position].1.clone_from(value);
                        } else {
                            previous.remove(position);
                        }
                    } else if let Some(value) = value {
                        previous.push((name.clone(), value.clone()));
                    }
                }
            }
        }
        let mut sections = Vec::new();
        for (name, value) in desired {
            if !previous
                .iter()
                .any(|(key, old)| key == name && old == value)
            {
                sections.push((name.clone(), Some(value.clone())));
            }
        }
        for (name, _) in previous {
            if !desired.iter().any(|(key, _)| key == &name) {
                sections.push((name, None));
            }
        }
        (!sections.is_empty()).then_some(SystemMessage {
            content: Vec::new(),
            sections,
            timestamp_ms,
        })
    }
}
