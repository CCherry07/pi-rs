use crate::{PluginError, ProviderError, ToolError};

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Plugin(#[from] PluginError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("duplicate plugin id: {0}")]
    DuplicatePlugin(String),
    #[error("duplicate provider plugin id: {0}")]
    DuplicateProviderPlugin(String),
    #[error("duplicate tool name: {0}")]
    DuplicateTool(String),
    #[error("duplicate provider id: {0}")]
    DuplicateProvider(String),
    #[error("duplicate provider override: {0}")]
    DuplicateProviderOverride(String),
    #[error("duplicate model: {0}")]
    DuplicateModel(String),
    #[error("model {model} references unknown provider {provider}")]
    ModelProviderNotFound { provider: String, model: String },
    #[error("duplicate command name: {0}")]
    DuplicateCommand(String),
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
}
