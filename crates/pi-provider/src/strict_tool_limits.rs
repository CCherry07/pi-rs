//! Shared bounded session memory for providers with strict-tool request limits.

use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use futures::StreamExt;
use pi_core::{ConstrainedSampling, StreamEvent, ToolSchemaStrict, ToolSpec};
use pi_plugin::ProviderStream;
use serde_json::Value;

const MAX_SESSIONS: usize = 10_000;

#[derive(Default)]
struct Sessions {
    order: VecDeque<String>,
    ids: HashSet<String>,
}

fn sessions() -> &'static Mutex<Sessions> {
    static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();
    SESSIONS.get_or_init(Mutex::default)
}

pub fn has_exceeded_strict_tool_limits(session_id: Option<&str>) -> bool {
    session_id.is_some_and(|id| {
        sessions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ids
            .contains(id)
    })
}

pub fn mark_strict_tool_limits_exceeded(session_id: Option<&str>) {
    let Some(id) = session_id else {
        return;
    };
    let mut sessions = sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if sessions.ids.insert(id.into()) {
        sessions.order.push_back(id.into());
    }
    if sessions.ids.len() > MAX_SESSIONS
        && let Some(oldest) = sessions.order.pop_front()
    {
        sessions.ids.remove(&oldest);
    }
}

/// Embeddings may release session affinity state when a session ends.
pub fn clear_strict_tool_limits(session_id: Option<&str>) {
    let mut sessions = sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(id) = session_id {
        sessions.ids.remove(id);
        sessions.order.retain(|entry| entry != id);
    } else {
        *sessions = Sessions::default();
    }
}

pub fn prefers_json_schema_sampling(tool: &ToolSpec) -> bool {
    matches!(
        tool.constrained_sampling,
        Some(ConstrainedSampling::JsonSchema {
            strict: ToolSchemaStrict::Prefer
        })
    )
}

pub fn disable_preferred_strict_tools(tools: &mut [ToolSpec]) {
    for tool in tools {
        if prefers_json_schema_sampling(tool) {
            tool.constrained_sampling = None;
        }
    }
}

pub fn is_strict_tool_limit_error(message: &str) -> bool {
    [
        "The compiled grammar is too large",
        "Too many strict tools (",
        "Schemas contains too many parameters with union types (",
    ]
    .iter()
    .any(|limit| message.contains(limit))
}

/// Attach fallback evidence to the initial message metadata, before content begins.
pub fn with_provider_diagnostic(
    stream: ProviderStream,
    diagnostic: Option<Value>,
) -> ProviderStream {
    let Some(diagnostic) = diagnostic else {
        return stream;
    };
    Box::pin(stream.map(move |event| {
        event.map(|mut event| {
            if let StreamEvent::Start { metadata } = &mut event {
                metadata
                    .diagnostics
                    .get_or_insert_default()
                    .push(diagnostic.clone());
            }
            event
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_memory_is_session_scoped_and_can_be_released() {
        let id = "pi-provider-strict-limit-test";
        clear_strict_tool_limits(Some(id));
        assert!(!has_exceeded_strict_tool_limits(Some(id)));
        mark_strict_tool_limits_exceeded(Some(id));
        assert!(has_exceeded_strict_tool_limits(Some(id)));
        assert!(!has_exceeded_strict_tool_limits(None));
        clear_strict_tool_limits(Some(id));
        assert!(!has_exceeded_strict_tool_limits(Some(id)));
    }
}
