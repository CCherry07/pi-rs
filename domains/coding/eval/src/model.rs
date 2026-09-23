use std::path::PathBuf;

use pi_eval::EvalCase;

/// Coding resource-loading requirements for one evaluation case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodingEvalOptions {
    pub discover_extensions: bool,
    pub requires_js_host: bool,
}

/// A generic evaluation case with explicit Coding preparation options.
#[derive(Clone)]
pub struct CodingEvalCase {
    pub case: EvalCase,
    pub options: CodingEvalOptions,
}

impl CodingEvalCase {
    pub fn new(case: EvalCase) -> Self {
        Self {
            case,
            options: CodingEvalOptions::default(),
        }
    }

    pub fn discover_extensions(mut self, discover: bool) -> Self {
        self.options.discover_extensions = discover;
        self
    }

    pub fn requires_js_host(mut self, required: bool) -> Self {
        self.options.requires_js_host = required;
        self
    }
}

impl From<EvalCase> for CodingEvalCase {
    fn from(case: EvalCase) -> Self {
        Self::new(case)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodingEvalSystemPrompt {
    #[default]
    Default,
    WithoutPiDocumentation,
}

/// Product configuration that varies across comparative Coding runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingEvalVariant {
    pub name: String,
    pub system_prompt: CodingEvalSystemPrompt,
    pub extensions: Vec<String>,
    pub native_plugins: Vec<PathBuf>,
}

impl CodingEvalVariant {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            system_prompt: CodingEvalSystemPrompt::Default,
            extensions: Vec::new(),
            native_plugins: Vec::new(),
        }
    }

    pub fn system_prompt(mut self, treatment: CodingEvalSystemPrompt) -> Self {
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
}

impl From<String> for CodingEvalVariant {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

impl From<&str> for CodingEvalVariant {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}
