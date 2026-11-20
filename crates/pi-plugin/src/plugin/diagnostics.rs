//! Shared callback failures and diagnostic collection.

use crate::{AgentHook, PluginContextError, PluginId, SessionHook};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {plugin_id} failed in {hook}: {message}")]
    Hook {
        plugin_id: PluginId,
        hook: &'static str,
        source_path: Option<String>,
        message: String,
    },
    #[error("plugin failed: {0}")]
    Failure(String),
    #[error("plugin registration failed: {0}")]
    Registration(String),
    #[error(transparent)]
    Context(#[from] PluginContextError),
}

impl PluginError {
    /// Keep an adapter's typed hook identity instead of stringifying it into a second wrapper.
    pub fn with_hook(self, plugin_id: PluginId, hook: &'static str) -> Self {
        match self {
            error @ Self::Hook { .. } => error,
            error => Self::Hook {
                plugin_id,
                hook,
                source_path: None,
                message: error.to_string(),
            },
        }
    }

    pub fn diagnostic_message(&self) -> String {
        match self {
            Self::Hook { message, .. } | Self::Failure(message) | Self::Registration(message) => {
                message.clone()
            }
            Self::Context(error) => error.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDiagnostic {
    pub plugin_id: PluginId,
    pub hook: PluginHook,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Original typed error message for protocol consumers; display context stays in message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_message: Option<String>,
    pub message: String,
}

pub type PluginDiagnosticObserver = Arc<dyn Fn(&PluginDiagnostic) + Send + Sync>;

#[derive(Default)]
struct DiagnosticState {
    diagnostics: Vec<PluginDiagnostic>,
    observers: BTreeMap<u64, PluginDiagnosticObserver>,
    next_id: u64,
}

/// A generation-local diagnostic subscription; dropping it releases its callback.
#[must_use]
pub struct PluginDiagnosticSubscription {
    state: Weak<Mutex<DiagnosticState>>,
    id: u64,
}

impl PluginDiagnosticSubscription {
    pub fn close(&self) {
        if let Some(state) = self.state.upgrade() {
            let removed = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .observers
                .remove(&self.id);
            drop(removed);
        }
    }
}
impl Drop for PluginDiagnosticSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone, Default)]
pub(crate) struct PluginDiagnosticSink {
    state: Arc<Mutex<DiagnosticState>>,
    source_path: Option<String>,
}

impl PluginDiagnosticSink {
    pub(crate) fn with_source(&self, source_path: Option<String>) -> Self {
        Self {
            state: self.state.clone(),
            source_path,
        }
    }

    pub(crate) fn observe(
        &self,
        observer: PluginDiagnosticObserver,
    ) -> PluginDiagnosticSubscription {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id;
        state.next_id += 1;
        state.observers.insert(id, observer);
        PluginDiagnosticSubscription {
            state: Arc::downgrade(&self.state),
            id,
        }
    }
    pub(crate) fn record(
        &self,
        plugin_id: PluginId,
        hook: impl Into<PluginHook>,
        generation: Option<u64>,
        message: impl Into<String>,
    ) {
        let diagnostic = PluginDiagnostic {
            plugin_id,
            hook: hook.into(),
            generation,
            source_path: self.source_path.clone(),
            raw_message: None,
            message: message.into(),
        };
        self.publish(diagnostic);
    }

    pub(crate) fn record_failure(
        &self,
        plugin_id: PluginId,
        hook: impl Into<PluginHook>,
        generation: Option<u64>,
        error: &PluginError,
    ) {
        let (plugin_id, hook, source_path) = match error {
            PluginError::Hook {
                plugin_id,
                hook,
                source_path,
                ..
            } => (
                plugin_id.clone(),
                PluginHook::from(*hook),
                source_path.clone().or_else(|| self.source_path.clone()),
            ),
            _ => (plugin_id, hook.into(), self.source_path.clone()),
        };
        self.publish(PluginDiagnostic {
            plugin_id,
            hook,
            generation,
            source_path,
            raw_message: Some(error.diagnostic_message()),
            message: error.to_string(),
        });
    }

    fn publish(&self, diagnostic: PluginDiagnostic) {
        let observers = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.diagnostics.push(diagnostic.clone());
            state.observers.values().cloned().collect::<Vec<_>>()
        };
        // Observers may query diagnostics or publish session events. Never retain
        // the diagnostic mutex while invoking them; an observer cannot fail a hook.
        for observer in observers {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(&diagnostic)));
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<PluginDiagnostic> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diagnostics
            .clone()
    }

    pub(crate) fn take(&self) -> Vec<PluginDiagnostic> {
        std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .diagnostics,
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
