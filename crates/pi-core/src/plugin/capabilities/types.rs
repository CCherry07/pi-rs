//! Shared plugin-capability values, options, and errors.

use serde::{Deserialize, Serialize};

use crate::{ModelSpec, ThinkingLevel};

/// Product surface currently presenting a plugin-backed session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PresentationMode {
    Tui,
    Print,
    Json,
    Rpc,
}

/// Presentation-neutral severity for a plugin notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

impl NoticeLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub context_window: u64,
    pub percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModel {
    pub model: ModelSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactOptions {
    pub custom_instructions: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewSessionOptions {
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForkPosition {
    #[default]
    Before,
    At,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkOptions {
    pub position: ForkPosition,
}

impl Default for ForkOptions {
    fn default() -> Self {
        Self {
            position: ForkPosition::Before,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigateTreeOptions {
    pub summarize: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageDelivery {
    #[default]
    Steer,
    FollowUp,
    NextTurn,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageOptions {
    #[serde(default)]
    pub trigger_turn: Option<bool>,
    #[serde(default)]
    pub deliver_as: Option<MessageDelivery>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendUserMessageOptions {
    #[serde(default)]
    pub deliver_as: Option<MessageDelivery>,
    #[serde(default)]
    pub expand_prompt_templates: bool,
}

/// The capability set attached to one plugin callback invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginContextScope {
    Base,
    Command,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginContextError {
    #[error("plugin context has retired")]
    Retired,
    #[error("plugin context is not bound to a session")]
    Unbound,
    #[error("{0} is only available in a plugin command context")]
    CommandOnly(&'static str),
    #[error("plugin context capability is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid plugin context operation: {0}")]
    Invalid(String),
    #[error("plugin context operation failed: {0}")]
    Failed(String),
}

pub type PluginContextResult<T> = Result<T, PluginContextError>;

pub(super) fn unbound<T>() -> PluginContextResult<T> {
    Err(PluginContextError::Unbound)
}
