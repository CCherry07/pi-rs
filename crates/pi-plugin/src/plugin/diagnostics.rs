//! Shared callback failures and diagnostic collection.

use crate::{AgentHook, PluginContextError, PluginId, SessionHook};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {plugin_id} failed in {hook}: {message}")]
    Hook {
        plugin_id: PluginId,
        hook: &'static str,
        message: String,
    },
    #[error("plugin failed: {0}")]
    Failure(String),
    #[error("plugin registration failed: {0}")]
    Registration(String),
    #[error(transparent)]
    Context(#[from] PluginContextError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDiagnostic {
    pub plugin_id: PluginId,
    pub hook: PluginHook,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    pub message: String,
}

#[derive(Clone, Default)]
pub(crate) struct PluginDiagnosticSink {
    diagnostics: Arc<Mutex<Vec<PluginDiagnostic>>>,
}

impl PluginDiagnosticSink {
    pub(crate) fn record(
        &self,
        plugin_id: PluginId,
        hook: impl Into<PluginHook>,
        generation: Option<u64>,
        message: impl Into<String>,
    ) {
        self.diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PluginDiagnostic {
                plugin_id,
                hook: hook.into(),
                generation,
                message: message.into(),
            });
    }

    pub(crate) fn snapshot(&self) -> Vec<PluginDiagnostic> {
        self.diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn take(&self) -> Vec<PluginDiagnostic> {
        std::mem::take(
            &mut *self
                .diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

define_hooks! {
    /// Provider request lifecycle callbacks. Providers retain their independent driver.
    ProviderHook {
        BeforeProviderRequest => "before_provider_request",
        BeforeProviderHeaders => "before_provider_headers",
        AfterProviderResponse => "after_provider_response",
    }
}

/// A callback or plugin-defined background operation that emitted a diagnostic.
/// Serializes as the existing hook-name string, including custom operation names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginHook {
    Agent(AgentHook),
    Session(SessionHook),
    Provider(ProviderHook),
    Custom(String),
}

impl PluginHook {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Agent(hook) => hook.as_str(),
            Self::Session(hook) => hook.as_str(),
            Self::Provider(hook) => hook.as_str(),
            Self::Custom(name) => name,
        }
    }
}

impl From<AgentHook> for PluginHook {
    fn from(hook: AgentHook) -> Self {
        Self::Agent(hook)
    }
}

impl From<SessionHook> for PluginHook {
    fn from(hook: SessionHook) -> Self {
        Self::Session(hook)
    }
}

impl From<ProviderHook> for PluginHook {
    fn from(hook: ProviderHook) -> Self {
        Self::Provider(hook)
    }
}

impl From<&str> for PluginHook {
    fn from(name: &str) -> Self {
        if let Some(hook) = AgentHook::from_name(name) {
            Self::Agent(hook)
        } else if let Some(hook) = SessionHook::from_name(name) {
            Self::Session(hook)
        } else if let Some(hook) = ProviderHook::from_name(name) {
            Self::Provider(hook)
        } else {
            Self::Custom(name.to_owned())
        }
    }
}

impl std::fmt::Display for PluginHook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diagnostics_preserve_hook_names_and_optional_generation_on_the_wire() {
        for (name, hook) in [
            ("agent_start", PluginHook::Agent(AgentHook::AgentStart)),
            ("session_start", PluginHook::Session(SessionHook::Start)),
            (
                "before_provider_headers",
                PluginHook::Provider(ProviderHook::BeforeProviderHeaders),
            ),
            (
                "background_review",
                PluginHook::Custom("background_review".into()),
            ),
        ] {
            let legacy = json!({"pluginId": "fixture", "hook": name, "message": "failure"});
            let mut diagnostic: PluginDiagnostic = serde_json::from_value(legacy.clone()).unwrap();
            assert_eq!(diagnostic.hook, hook);
            assert_eq!(PluginHook::from(name), hook);
            assert_eq!(diagnostic.hook.as_str(), name);
            assert_eq!(diagnostic.generation, None);
            assert_eq!(serde_json::to_value(&diagnostic).unwrap(), legacy);
            diagnostic.generation = Some(7);
            let value = serde_json::to_value(&diagnostic).unwrap();
            assert_eq!(value["generation"], 7);
            assert_eq!(
                serde_json::from_value::<PluginDiagnostic>(value).unwrap(),
                diagnostic
            );
        }
    }
}
